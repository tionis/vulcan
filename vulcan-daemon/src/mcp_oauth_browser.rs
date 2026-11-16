//! Bounded, single-use browser transactions for local MCP OAuth issuers.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::prelude::{Engine as _, BASE64_URL_SAFE_NO_PAD};
use subtle::ConstantTimeEq;

use crate::http_policy::mcp_oauth_redirect_target;
use crate::mcp_grant_expiry::{
    GrantClock, CUSTOM_EXPIRY, DEFAULT_EXPIRY, EXPIRY_DATE_FIELD, EXPIRY_FIELD, EXPIRY_PRESETS,
};
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
    /// The time and time zone in which expiry choices are shown.
    pub clock: GrantClock,
}

/// The consent page's only stylesheet. The page runs no script; its
/// Content-Security-Policy admits this stylesheet by hash.
const CONSENT_STYLE: &str = r#"
:root{color-scheme:light dark;--bg:#f4f4f1;--card:#fff;--fg:#1c1c1a;--muted:#62625c;--line:#dedcd5;--accent:#2f5d50;--accent-fg:#fff;--soft:#eef3f1;--warn-bg:#fff6e0;--warn-line:#e7c36a;--warn-fg:#5a4205;--focus:#2f7d68}
@media (prefers-color-scheme:dark){:root{--bg:#141513;--card:#1d1f1c;--fg:#ecebe6;--muted:#a3a29b;--line:#33352f;--accent:#7cc2a8;--accent-fg:#0d1a15;--soft:#22302b;--warn-bg:#2d2512;--warn-line:#7a6326;--warn-fg:#f1dca4;--focus:#9fd8c3}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
main{max-width:40rem;margin:0 auto;padding:2.5rem 1rem 3rem}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:1.75rem;box-shadow:0 1px 2px rgb(0 0 0/.04),0 8px 24px rgb(0 0 0/.06)}
.eyebrow{margin:0;color:var(--muted);font-size:.8rem;font-weight:600;letter-spacing:.08em;text-transform:uppercase}
h1{margin:.25rem 0 1.25rem;font-size:1.5rem;line-height:1.25}
h2{margin:0 0 .75rem;font-size:.8rem;font-weight:600;letter-spacing:.08em;text-transform:uppercase;color:var(--muted)}
section{border-top:1px solid var(--line);padding:1.25rem 0}
.destination{background:var(--soft);border-radius:10px;padding:.9rem 1rem;margin-bottom:1rem}
.destination span{display:block;color:var(--muted);font-size:.85rem}
.destination strong{font-size:1.15rem;word-break:break-all}
.warning{margin:0 0 1.25rem;background:var(--warn-bg);border:1px solid var(--warn-line);color:var(--warn-fg);border-radius:10px;padding:.75rem 1rem;font-size:.95rem}
dl{display:grid;grid-template-columns:minmax(7rem,max-content) 1fr;gap:.5rem 1rem;margin:0}
dt{color:var(--muted)}
dd{margin:0;min-width:0;overflow-wrap:anywhere}
.chips{display:flex;flex-wrap:wrap;gap:.35rem}
.chip{display:inline-block;border:1px solid var(--line);border-radius:999px;padding:.05rem .6rem;font-size:.85rem;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
label.pack{display:inline-flex;align-items:center;gap:.4rem;border:1px solid var(--line);border-radius:999px;padding:.2rem .75rem .2rem .55rem;cursor:pointer}
label.pack:has(input:checked){border-color:var(--accent);background:var(--soft)}
fieldset{border:0;margin:0;padding:0;min-width:0}
legend{padding:0}
fieldset.vault{border:1px solid var(--line);border-radius:10px;padding:.75rem 1rem;margin-bottom:.75rem}
fieldset.vault:has(> legend input:checked){border-color:var(--accent)}
fieldset.vault legend{padding:0 .35rem;font-weight:600}
.field{display:grid;gap:.35rem;margin:.6rem 0}
.field>span{color:var(--muted);font-size:.9rem}
input[type=text],input:not([type]),input[type=date]{font:inherit;color:inherit;background:var(--card);border:1px solid var(--line);border-radius:8px;padding:.45rem .65rem;max-width:100%}
input:focus-visible,button:focus-visible{outline:2px solid var(--focus);outline-offset:2px}
input[type=checkbox],input[type=radio]{accent-color:var(--accent)}
.note{color:var(--muted);font-size:.875rem;margin:.75rem 0 0}
.choices{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:.5rem}
.choice{position:relative;display:block;cursor:pointer}
.choice input{position:absolute;opacity:0;inset:0;margin:0;cursor:pointer}
.choice span{display:block;height:100%;border:1px solid var(--line);border-radius:10px;padding:.55rem .75rem}
.choice strong{display:block;font-weight:600}
.choice small{color:var(--muted)}
.choice input:checked+span{border-color:var(--accent);background:var(--soft);box-shadow:inset 0 0 0 1px var(--accent)}
.choice input:focus-visible+span{outline:2px solid var(--focus);outline-offset:2px}
.custom-date{display:none}
.expiry:has(input[value=custom]:checked) .custom-date{display:grid}
.actions{display:flex;justify-content:flex-end;gap:.75rem;border-top:1px solid var(--line);padding-top:1.25rem}
button{font:inherit;font-weight:600;border-radius:10px;padding:.6rem 1.4rem;cursor:pointer;border:1px solid var(--line);background:transparent;color:var(--fg)}
button.approve{background:var(--accent);border-color:var(--accent);color:var(--accent-fg)}
code{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.9em}
@media (max-width:30rem){.card{padding:1.25rem;border-radius:0;border-left:0;border-right:0}main{padding:0 0 2rem}dl{grid-template-columns:1fr}dt{margin-top:.4rem}.actions{flex-direction:column-reverse}button{width:100%}.choices{grid-template-columns:repeat(2,minmax(0,1fr))}}
"#;

fn consent_style_hash() -> String {
    use sha2::{Digest as _, Sha256};
    format!(
        "'sha256-{}'",
        base64::prelude::BASE64_STANDARD.encode(Sha256::digest(CONSENT_STYLE.as_bytes()))
    )
}

fn render_expiry_control(clock: &GrantClock) -> String {
    use std::fmt::Write as _;

    let mut choices = String::new();
    for preset in EXPIRY_PRESETS {
        let until = clock
            .preset_end(preset)
            .map(|end| clock.describe(end))
            .unwrap_or_default();
        let checked = if preset.value == DEFAULT_EXPIRY {
            " checked"
        } else {
            ""
        };
        write!(
            choices,
            "<label class=\"choice\"><input type=\"radio\" name=\"{EXPIRY_FIELD}\" value=\"{}\"{checked}><span><strong>{}</strong><small>until {}</small></span></label>",
            preset.value,
            preset.label,
            html_escape(&until),
        )
        .expect("writing to a String cannot fail");
    }
    let today = clock.today().format("%Y-%m-%d");
    let latest = clock.latest_custom_date().format("%Y-%m-%d");
    format!(
        "<section><fieldset class=\"expiry\"><legend><h2>Expires</h2></legend>\
         <div class=\"choices\">{choices}<label class=\"choice\"><input type=\"radio\" name=\"{EXPIRY_FIELD}\" value=\"{CUSTOM_EXPIRY}\"><span><strong>Pick a date</strong><small>up to 1 year</small></span></label></div>\
         <label class=\"field custom-date\"><span>Last day of access</span><input type=\"date\" name=\"{EXPIRY_DATE_FIELD}\" min=\"{today}\" max=\"{latest}\"></label>\
         <p class=\"note\">A chosen date lasts until the end of that day. Times are in this server's time zone ({}). You can revoke a connection at any time with <code>vulcan mcp connections revoke</code>.</p>\
         </fieldset></section>",
        html_escape(&clock.zone_label()),
    )
}

/// Read-only chips, one per item.
fn chips<'a>(items: impl IntoIterator<Item = &'a String>) -> String {
    use std::fmt::Write as _;

    items.into_iter().fold(String::new(), |mut output, item| {
        write!(output, "<span class=\"chip\">{}</span>", html_escape(item))
            .expect("writing to a String cannot fail");
        output
    })
}

/// The vault, permission profile, and tool pack part of the consent page.
fn render_access(page: &ConsentPage<'_>) -> String {
    use std::fmt::Write as _;

    let Some(named) = page.named_runtime else {
        return format!(
            "<dl><dt>Vault</dt><dd>{}</dd><dt>Permission profile</dt><dd>{}</dd><dt>Tool packs</dt><dd><span class=\"chips\">{}</span></dd></dl>",
            html_escape(page.fallback_vault_root),
            html_escape(page.profile),
            chips(page.packs),
        );
    };
    let multi = named.vaults.len() > 1;
    let mut vaults = String::new();
    if multi {
        vaults.push_str("<p class=\"note\">Choose the one vault this connection may use.</p>");
    }
    for (wiki_id, vault) in &named.vaults {
        let wiki = html_escape(wiki_id.as_str());
        let profile_field = if multi {
            format!("permission_profile_{wiki}")
        } else {
            "permission_profile".to_string()
        };
        let packs = vault
            .eligible_tool_packs
            .iter()
            .fold(String::new(), |mut output, pack| {
                let field = if multi {
                    format!("pack_{wiki}_{pack}")
                } else {
                    format!("pack_{pack}")
                };
                write!(
                    output,
                    "<label class=\"pack\"><input type=\"checkbox\" name=\"{}\" value=\"on\" checked> {}</label>",
                    html_escape(&field),
                    html_escape(pack)
                )
                .expect("writing to a String cannot fail");
                output
            });
        let root = html_escape(&vault.paths.vault_root().display().to_string());
        let controls = format!(
            "<label class=\"field\"><span>Permission profile</span><input name=\"{profile_field}\" value=\"{default}\" list=\"profiles_{wiki}\" required><datalist id=\"profiles_{wiki}\"><option value=\"{default}\"><option value=\"{ceiling}\"></datalist></label>\
             <div class=\"field\"><span>Tool packs</span><div class=\"chips\">{packs}</div></div>",
            default = html_escape(&vault.default_profile),
            ceiling = html_escape(&vault.ceiling_profile),
        );
        if multi {
            write!(
                vaults,
                "<fieldset class=\"vault\"><legend><label><input type=\"radio\" name=\"wiki_id\" value=\"{wiki}\" required> {wiki}</label></legend><p class=\"note\">{root}</p>{controls}</fieldset>"
            )
        } else {
            write!(
                vaults,
                "<dl><dt>Vault</dt><dd>{wiki} <span class=\"note\">{root}</span></dd></dl>{controls}"
            )
        }
        .expect("writing to a String cannot fail");
    }
    vaults
}

#[must_use]
pub fn render_consent_page(page: &ConsentPage<'_>) -> McpHttpResponse {
    let access = render_access(page);
    let expiry = page
        .named_runtime
        .map_or_else(String::new, |_| render_expiry_control(&page.clock));
    // Registration is open, and a client chooses its own name, so the page
    // names where the authorization code goes: that is what a lookalike
    // client cannot fake.
    let target = mcp_oauth_redirect_target(&page.pending.redirect_uri);
    let destination = target.as_ref().map_or_else(
        || html_escape(&page.pending.redirect_uri),
        |target| html_escape(&target.describe()),
    );
    let form_action = target.as_ref().map_or_else(
        || "'self'".to_string(),
        |target| format!("'self' {}", target.csp_source()),
    );
    let scopes = chips(&page.pending.scopes);
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Authorize Vulcan MCP</title><style>{CONSENT_STYLE}</style></head>\
         <body><main><div class=\"card\">\
         <p class=\"eyebrow\">Vulcan MCP</p><h1>Authorize this MCP connection?</h1>\
         <div class=\"destination\"><span>Sends the authorization to</span><strong>{destination}</strong></div>\
         <p class=\"warning\"><strong>Approve only if you started this connection yourself, just now, from the client that receives it.</strong></p>\
         <form method=\"post\" action=\"/oauth/consent\">\
         <section><h2>Request</h2><dl><dt>Client</dt><dd>{}</dd><dt>Identity</dt><dd>{}</dd>\
         <dt>Resource</dt><dd>{}</dd><dt>OAuth scopes</dt><dd><span class=\"chips\">{scopes}</span></dd></dl></section>\
         <section><h2>Access</h2>{access}\
         <p class=\"note\">Tool packs control which tools the client sees. The permission profile remains the authority ceiling.</p></section>\
         {expiry}\
         <input type=\"hidden\" name=\"transaction\" value=\"{}\">\
         <input type=\"hidden\" name=\"csrf_token\" value=\"{}\">\
         <div class=\"actions\"><button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button>\
         <button class=\"approve\" type=\"submit\" name=\"decision\" value=\"approve\">Approve</button></div>\
         </form></div></main></body></html>",
        html_escape(page.client_name),
        html_escape(&page.pending.subject),
        html_escape(&page.pending.resource),
        html_escape(page.transaction_id),
        html_escape(&page.pending.csrf_token),
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
                // Browsers apply form-action to the redirect after the form
                // posts, so the client's callback must be allowed as well.
                format!(
                    "default-src 'none'; style-src {}; form-action {form_action}; frame-ancestors 'none'; base-uri 'none'",
                    consent_style_hash()
                ),
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
            clock: GrantClock::utc(1_794_830_400),
        });
        let csp = response
            .extra_headers
            .iter()
            .find(|(name, _)| name == "Content-Security-Policy")
            .map(|(_, value)| value.clone())
            .unwrap();
        assert!(
            csp.contains("form-action 'self' https://client.example;"),
            "{csp}"
        );
        let html = String::from_utf8(response.body).unwrap();
        assert!(html.contains("Sends the authorization to</span><strong>client.example</strong>"));
        // The direct issuer has no grant lifetime to choose.
        assert!(!html.contains("name=\"expiry\""));
        assert!(html.contains("Approve only if you started this connection yourself"));
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
    fn consent_for_a_loopback_client_names_this_computer_and_allows_only_that_origin() {
        let mut transaction = consent();
        transaction.redirect_uri = "http://127.0.0.1:33418/callback".into();
        let response = render_consent_page(&ConsentPage {
            transaction_id: "transaction",
            pending: &transaction,
            client_name: "Claude",
            fallback_vault_root: "/vault",
            profile: "readonly",
            packs: &[],
            named_runtime: None,
            clock: GrantClock::utc(1_794_830_400),
        });
        let csp = response
            .extra_headers
            .iter()
            .find(|(name, _)| name == "Content-Security-Policy")
            .map(|(_, value)| value.clone())
            .unwrap();
        assert!(
            csp.contains("form-action 'self' http://127.0.0.1:33418;"),
            "{csp}"
        );
        let html = String::from_utf8(response.body).unwrap();
        assert!(html.contains("this computer (http://127.0.0.1:33418)"));
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
            clock: GrantClock::utc(1_794_830_400),
        });
        let html = String::from_utf8(response.body).unwrap();
        assert_eq!(html.matches("name=\"wiki_id\"").count(), 2);
        assert!(html.contains("permission_profile_personal"));
        assert!(html.contains("permission_profile_work"));
        assert!(html.contains("pack_personal_search"));
        assert!(html.contains("pack_work_search"));
        assert!(!html.contains("expiry_days"));
        assert_eq!(
            html.matches("name=\"expiry\"").count(),
            EXPIRY_PRESETS.len() + 1
        );
        assert!(html.contains("value=\"30d\" checked"));
        assert!(html.contains("<small>until 16 Dec</small>"));
        assert!(html.contains("<small>until 13:00</small>"));
        assert!(html.contains("<small>until 16 Nov 2027</small>"));
        assert!(html.contains("name=\"expiry_date\" min=\"2026-11-16\" max=\"2027-11-16\""));
        assert!(html.contains("this server's time zone (UTC)"));
    }

    #[test]
    fn consent_stylesheet_is_admitted_by_hash_and_the_page_runs_no_script() {
        let transaction = consent();
        let response = render_consent_page(&ConsentPage {
            transaction_id: "transaction",
            pending: &transaction,
            client_name: "client",
            fallback_vault_root: "/vault",
            profile: "readonly",
            packs: &[],
            named_runtime: None,
            clock: GrantClock::utc(1_794_830_400),
        });
        let csp = response
            .extra_headers
            .iter()
            .find_map(|(name, value)| (name == "Content-Security-Policy").then_some(value.clone()))
            .unwrap();
        assert!(
            csp.starts_with("default-src 'none'; style-src 'sha256-"),
            "{csp}"
        );
        assert!(!csp.contains("unsafe-inline"));
        assert!(!csp.contains("script-src"));
        let html = String::from_utf8(response.body).unwrap();
        assert_eq!(html.matches("<style>").count(), 1);
        assert!(!html.contains(" style=\""));
        assert!(!html.contains("<script"));
        // Deny comes first, so pressing Enter in a field does not approve.
        assert!(html.find("value=\"deny\"").unwrap() < html.find("value=\"approve\"").unwrap());
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
