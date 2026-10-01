//! OAuth login for forge adapters (Roadmap 12.21.5).
//!
//! Authorization code with PKCE (S256) and a loopback redirect, as in RFC 8252,
//! using a public client: only a client ID is needed, never a client secret.
//! Tokens live in one 0600 file per forge origin and client under the
//! device-local state directory, never in a vault, and refresh on use. The
//! token endpoint is only ever the configured forge origin, and redirects are
//! never followed, so a credential cannot be steered to another host.

use super::ForgeConfig;
use crate::sync_state::SyncStateStore;
use crate::{durable_file, AppError};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

const TOKEN_FILE_VERSION: u32 = 1;
const MAX_TOKEN_FILE_BYTES: u64 = 16 * 1024;
const MAX_TOKEN_RESPONSE_BYTES: u64 = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 8 * 1024;
const MAX_MESSAGE_CHARS: usize = 200;
/// Treat a token as expired this long before it really is.
const EXPIRY_SKEW_SECONDS: u64 = 60;
const DEFAULT_LIFETIME_SECONDS: u64 = 3600;
/// Default time to wait for the browser to return.
pub const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTokens {
    version: u32,
    origin: String,
    client_id: String,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    expires_at_unix: u64,
    obtained_at_unix: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Scheme, host, and port of the forge, lowercased: the unit a login belongs to.
fn forge_origin(config: &ForgeConfig) -> Result<String, AppError> {
    let url = Url::parse(&config.url)
        .map_err(|_| AppError::operation("forge URL must be a valid http(s) URL"))?;
    Ok(url.origin().ascii_serialization())
}

fn client_id(config: &ForgeConfig) -> Result<&str, AppError> {
    config.oauth_client_id.as_deref().ok_or_else(|| {
        AppError::operation(
            "no OAuth client ID is configured; set one with `sync forge set` or `sync forge init`",
        )
    })
}

/// Where tokens for one forge origin and client live: a hashed file name keeps
/// the path free of anything attacker-influenced.
struct TokenStore {
    directory: PathBuf,
}

impl TokenStore {
    fn new(state: &SyncStateStore) -> Self {
        Self {
            directory: state.forge_oauth_dir(),
        }
    }

    fn path(&self, origin: &str, client_id: &str) -> PathBuf {
        let key = blake3::hash(format!("{origin}\n{client_id}").as_bytes()).to_hex();
        self.directory.join(format!("{}.json", &key.as_str()[..32]))
    }

    fn load(&self, origin: &str, client_id: &str) -> Result<Option<StoredTokens>, AppError> {
        let path = self.path(origin, client_id);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AppError::operation(error)),
        };
        if !metadata.is_file() || metadata.len() > MAX_TOKEN_FILE_BYTES {
            return Err(AppError::operation(
                "the stored login is not a bounded regular file",
            ));
        }
        let tokens: StoredTokens = serde_json::from_slice(
            &fs::read(&path).map_err(AppError::operation)?,
        )
        .map_err(|_| {
            AppError::operation("the stored login is unreadable; run `sync forge login` again")
        })?;
        if tokens.version != TOKEN_FILE_VERSION
            || tokens.origin != origin
            || tokens.client_id != client_id
        {
            return Err(AppError::operation(
                "the stored login does not match this forge; run `sync forge login` again",
            ));
        }
        Ok(Some(tokens))
    }

    fn save(&self, tokens: &StoredTokens) -> Result<(), AppError> {
        fs::create_dir_all(&self.directory).map_err(AppError::operation)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))
                .map_err(AppError::operation)?;
        }
        let bytes = Zeroizing::new(serde_json::to_vec(tokens).map_err(AppError::operation)?);
        // The temporary file is created owner-only and renamed into place.
        durable_file::replace(&self.path(&tokens.origin, &tokens.client_id), &bytes)
    }

    fn remove(&self, origin: &str, client_id: &str) -> Result<bool, AppError> {
        durable_file::remove(&self.path(origin, client_id))
    }
}

fn random_url_safe(bytes: usize) -> Result<String, AppError> {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer)
        .map_err(|error| AppError::operation(format!("no secure randomness: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

/// A PKCE verifier (86 characters, within RFC 7636's 43-128) and its S256 challenge.
fn pkce_pair() -> Result<(String, String), AppError> {
    let verifier = random_url_safe(64)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok((verifier, challenge))
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

/// A short, single-line message built from untrusted text.
fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_MESSAGE_CHARS)
        .collect()
}

fn endpoint(config: &ForgeConfig, path: &str) -> Result<Url, AppError> {
    let mut base = Url::parse(&config.url)
        .map_err(|_| AppError::operation("forge URL must be a valid http(s) URL"))?;
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    base.join(path)
        .map_err(|_| AppError::operation("failed to construct the forge OAuth endpoint"))
}

fn authorize_url(
    config: &ForgeConfig,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> Result<Url, AppError> {
    let (authorize, _) = config.kind.oauth_paths();
    let mut url = endpoint(config, authorize)?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url)
}

fn http_client(timeout: Duration) -> Result<Client, AppError> {
    // Never follow redirects: the token endpoint is the configured forge only.
    Client::builder()
        .timeout(timeout)
        .redirect(Policy::none())
        .build()
        .map_err(|error| {
            AppError::operation(format!("failed to configure the OAuth client: {error}"))
        })
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// POSTs a form to the token endpoint and returns fresh tokens.
fn request_tokens(
    client: &Client,
    config: &ForgeConfig,
    client_id: &str,
    origin: &str,
    form: &[(&str, &str)],
    previous_refresh: Option<&str>,
) -> Result<StoredTokens, AppError> {
    let (_, token_path) = config.kind.oauth_paths();
    let response = client
        .post(endpoint(config, token_path)?)
        .header("Accept", "application/json")
        .form(form)
        .send()
        .map_err(|error| {
            AppError::operation(format!(
                "the forge's token endpoint is unreachable: {error}"
            ))
        })?;
    let status = response.status();
    let mut body = Vec::new();
    response
        .take(MAX_TOKEN_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(AppError::operation)?;
    if body.len() as u64 > MAX_TOKEN_RESPONSE_BYTES {
        return Err(AppError::operation(
            "the token response exceeded its size limit",
        ));
    }
    let parsed: TokenResponse = serde_json::from_slice(&body).map_err(|_| {
        AppError::operation(format!(
            "the forge returned an unexpected token response (HTTP {status})"
        ))
    })?;
    if let Some(error) = parsed.error {
        let detail = parsed
            .error_description
            .map(|description| format!(": {}", sanitize(&description)))
            .unwrap_or_default();
        return Err(AppError::operation(format!(
            "the forge refused the token request ({}){detail}",
            sanitize(&error)
        )));
    }
    if !status.is_success() {
        return Err(AppError::operation(format!(
            "the forge refused the token request (HTTP {status})"
        )));
    }
    let access_token = parsed
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| AppError::operation("the forge's token response had no access token"))?;
    let now = now_unix();
    Ok(StoredTokens {
        version: TOKEN_FILE_VERSION,
        origin: origin.to_owned(),
        client_id: client_id.to_owned(),
        access_token,
        // A refresh that returns no new refresh token keeps the old one.
        refresh_token: parsed
            .refresh_token
            .filter(|token| !token.is_empty())
            .or_else(|| previous_refresh.map(str::to_owned)),
        expires_at_unix: now + parsed.expires_in.unwrap_or(DEFAULT_LIFETIME_SECONDS),
        obtained_at_unix: now,
    })
}

/// What the browser sent back to the loopback listener.
struct Callback {
    code: String,
}

/// Waits for exactly one valid redirect. Unrelated requests (a favicon fetch,
/// a probe) get a 404 and are ignored; a wrong `state` or an error aborts.
fn wait_for_callback(
    listener: &TcpListener,
    expected_state: &str,
    deadline: Instant,
) -> Result<Callback, AppError> {
    listener
        .set_nonblocking(true)
        .map_err(AppError::operation)?;
    loop {
        if Instant::now() >= deadline {
            return Err(AppError::operation(
                "timed out waiting for the browser to approve the login",
            ));
        }
        let Ok((mut stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        stream.set_nonblocking(false).map_err(AppError::operation)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(AppError::operation)?;
        let Some(target) = read_request_target(&mut stream) else {
            respond(&mut stream, 400, "Bad request.");
            continue;
        };
        let Ok(url) = Url::parse("http://127.0.0.1").and_then(|base| base.join(&target)) else {
            respond(&mut stream, 400, "Bad request.");
            continue;
        };
        if url.path() != "/" {
            respond(&mut stream, 404, "Not found.");
            continue;
        }
        let pairs = url.query_pairs().into_owned().collect::<Vec<_>>();
        let value = |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, v)| v.as_str())
        };
        if let Some(error) = value("error") {
            respond(
                &mut stream,
                400,
                "Login was not approved. You can close this tab.",
            );
            let detail = value("error_description")
                .map(|description| format!(": {}", sanitize(description)))
                .unwrap_or_default();
            return Err(AppError::operation(format!(
                "the forge reported an error ({}){detail}",
                sanitize(error)
            )));
        }
        let (Some(code), Some(state)) = (value("code"), value("state")) else {
            respond(&mut stream, 404, "Not found.");
            continue;
        };
        if !constant_time_eq(state, expected_state) {
            respond(
                &mut stream,
                400,
                "This login request was not started by Vulcan. You can close this tab.",
            );
            return Err(AppError::operation(
                "the redirect carried an unexpected `state`; the login was aborted",
            ));
        }
        if code.is_empty() || code.len() > 2048 {
            respond(&mut stream, 400, "Bad request.");
            return Err(AppError::operation(
                "the redirect carried an invalid authorization code",
            ));
        }
        respond(
            &mut stream,
            200,
            "Vulcan login complete. You can close this tab.",
        );
        return Ok(Callback {
            code: code.to_owned(),
        });
    }
}

/// The request-target of a `GET` request line, if the head arrives intact.
fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    while buffer.len() < MAX_REQUEST_BYTES && !buffer.windows(4).any(|window| window == b"\r\n\r\n")
    {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let head = String::from_utf8_lossy(&buffer);
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    (parts.next()? == "GET").then_some(())?;
    let target = parts.next()?;
    target.starts_with('/').then(|| target.to_owned())
}

fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let body = format!("<!doctype html><meta charset=utf-8><title>Vulcan</title><p>{message}</p>");
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeLoginReport {
    pub version: u32,
    pub origin: String,
    pub client_id: String,
    pub expires_in_seconds: u64,
    pub has_refresh_token: bool,
}

/// Runs the browser login. `open` receives the authorization URL (a caller
/// prints it and may open a browser); it must not block on the redirect.
pub fn forge_login(
    config: &ForgeConfig,
    timeout: Duration,
    open: &dyn Fn(&str),
) -> Result<ForgeLoginReport, AppError> {
    forge_login_with(
        &TokenStore::new(&SyncStateStore::user_default()?),
        config,
        timeout,
        open,
    )
}

fn forge_login_with(
    store: &TokenStore,
    config: &ForgeConfig,
    timeout: Duration,
    open: &dyn Fn(&str),
) -> Result<ForgeLoginReport, AppError> {
    let client_id = client_id(config)?;
    let origin = forge_origin(config)?;
    let (verifier, challenge) = pkce_pair()?;
    let state = random_url_safe(32)?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(AppError::operation)?;
    let redirect_uri = format!(
        "http://127.0.0.1:{}/",
        listener.local_addr().map_err(AppError::operation)?.port()
    );
    let url = authorize_url(config, client_id, &redirect_uri, &state, &challenge)?;
    open(url.as_str());
    let callback = wait_for_callback(&listener, &state, Instant::now() + timeout)?;
    let client = http_client(Duration::from_secs(30))?;
    let tokens = request_tokens(
        &client,
        config,
        client_id,
        &origin,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", &callback.code),
            ("redirect_uri", &redirect_uri),
            ("code_verifier", &verifier),
        ],
        None,
    )?;
    store.save(&tokens)?;
    Ok(ForgeLoginReport {
        version: super::SYNC_FORGE_REPORT_VERSION,
        origin,
        client_id: client_id.to_owned(),
        expires_in_seconds: tokens
            .expires_at_unix
            .saturating_sub(tokens.obtained_at_unix),
        has_refresh_token: tokens.refresh_token.is_some(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeOAuthStatus {
    pub origin: String,
    pub client_id: String,
    pub logged_in: bool,
    /// The access token has expired; it refreshes on next use when a refresh
    /// token exists.
    pub access_token_expired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_seconds: Option<i64>,
    pub has_refresh_token: bool,
}

/// Local, read-only login state; never contacts the forge or prints a token.
pub fn forge_oauth_status(config: &ForgeConfig) -> Result<Option<ForgeOAuthStatus>, AppError> {
    status_with(
        &TokenStore::new(&SyncStateStore::user_default()?),
        config,
        now_unix(),
    )
}

fn status_with(
    store: &TokenStore,
    config: &ForgeConfig,
    now: u64,
) -> Result<Option<ForgeOAuthStatus>, AppError> {
    let Some(client_id) = config.oauth_client_id.as_deref() else {
        return Ok(None);
    };
    let origin = forge_origin(config)?;
    let tokens = store.load(&origin, client_id)?;
    let remaining = tokens.as_ref().map(|tokens| {
        i64::try_from(tokens.expires_at_unix).unwrap_or(i64::MAX) - i64::try_from(now).unwrap_or(0)
    });
    Ok(Some(ForgeOAuthStatus {
        origin,
        client_id: client_id.to_owned(),
        logged_in: tokens.is_some(),
        access_token_expired: remaining.is_some_and(|seconds| seconds <= 0),
        expires_in_seconds: remaining,
        has_refresh_token: tokens
            .as_ref()
            .is_some_and(|tokens| tokens.refresh_token.is_some()),
    }))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeLogoutReport {
    pub version: u32,
    pub removed: bool,
    pub note: &'static str,
}

/// Deletes the local login. The grant itself is revoked in the forge's
/// application settings; Vulcan cannot revoke it for you.
pub fn forge_logout(config: &ForgeConfig) -> Result<ForgeLogoutReport, AppError> {
    let store = TokenStore::new(&SyncStateStore::user_default()?);
    let removed = store.remove(&forge_origin(config)?, client_id(config)?)?;
    Ok(ForgeLogoutReport {
        version: super::SYNC_FORGE_REPORT_VERSION,
        removed,
        note: "the local login was removed; revoke the app's access in the forge's Applications settings to invalidate the grant",
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    Oauth,
    Env,
}

/// A bearer token and where it came from. Cleared from memory when dropped.
pub struct ForgeCredential {
    pub token: Zeroizing<String>,
    pub source: CredentialSource,
}

impl std::fmt::Debug for ForgeCredential {
    /// Never prints the token, so a stray `{:?}` cannot leak it.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ForgeCredential")
            .field("token", &"[redacted]")
            .field("source", &self.source)
            .finish()
    }
}

/// Resolves the token for one forge: the OAuth login first (refreshing it when
/// it has expired), then the configured environment variable.
pub fn resolve_forge_credential(
    config: &ForgeConfig,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ForgeCredential, AppError> {
    resolve_with(
        &TokenStore::new(&SyncStateStore::user_default()?),
        config,
        env,
        now_unix(),
    )
}

fn resolve_with(
    store: &TokenStore,
    config: &ForgeConfig,
    env: &dyn Fn(&str) -> Option<String>,
    now: u64,
) -> Result<ForgeCredential, AppError> {
    let mut oauth_problem = None;
    if let Some(client_id) = config.oauth_client_id.as_deref() {
        match oauth_token(store, config, client_id, now) {
            Ok(Some(token)) => {
                return Ok(ForgeCredential {
                    token,
                    source: CredentialSource::Oauth,
                });
            }
            Ok(None) => {}
            Err(error) => oauth_problem = Some(error),
        }
    }
    if let Some(name) = config.token_env.as_deref() {
        if let Some(token) = env(name).filter(|value| !value.trim().is_empty()) {
            return Ok(ForgeCredential {
                token: Zeroizing::new(token.trim().to_owned()),
                source: CredentialSource::Env,
            });
        }
    }
    if let Some(error) = oauth_problem {
        return Err(error);
    }
    Err(AppError::operation(
        match (&config.oauth_client_id, &config.token_env) {
            (Some(_), Some(name)) => {
                format!("no forge credential: run `vulcan sync forge login`, or set ${name}")
            }
            (Some(_), None) => "no forge credential: run `vulcan sync forge login`".to_owned(),
            (None, Some(name)) => format!("forge API token variable `{name}` is not set"),
            (None, None) => "no forge credential is configured".to_owned(),
        },
    ))
}

/// A usable OAuth access token, refreshed when needed. `Ok(None)` means there
/// is no login for this forge and client.
fn oauth_token(
    store: &TokenStore,
    config: &ForgeConfig,
    client_id: &str,
    now: u64,
) -> Result<Option<Zeroizing<String>>, AppError> {
    let origin = forge_origin(config)?;
    let Some(tokens) = store.load(&origin, client_id)? else {
        return Ok(None);
    };
    if tokens.expires_at_unix > now + EXPIRY_SKEW_SECONDS {
        return Ok(Some(Zeroizing::new(tokens.access_token)));
    }
    let Some(refresh) = tokens.refresh_token.clone() else {
        return Err(AppError::operation(
            "the forge login has expired and has no refresh token; run `vulcan sync forge login`",
        ));
    };
    let client = http_client(Duration::from_secs(30))?;
    let refreshed = request_tokens(
        &client,
        config,
        client_id,
        &origin,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", &refresh),
        ],
        Some(&refresh),
    );
    match refreshed {
        Ok(new_tokens) => {
            store.save(&new_tokens)?;
            Ok(Some(Zeroizing::new(new_tokens.access_token)))
        }
        Err(error) => {
            // Refresh tokens rotate, so another process may have refreshed
            // first and invalidated ours; use its result if it is fresh.
            if let Some(current) = store.load(&origin, client_id)? {
                if current.refresh_token != tokens.refresh_token
                    && current.expires_at_unix > now + EXPIRY_SKEW_SECONDS
                {
                    return Ok(Some(Zeroizing::new(current.access_token)));
                }
            }
            Err(AppError::operation(format!(
                "could not refresh the forge login ({error}); run `vulcan sync forge login`"
            )))
        }
    }
}

/// Best-effort launch of the user's browser without a shell. The URL is always
/// shown to the user as well, so a failure here is harmless.
#[must_use]
pub fn open_in_browser(url: &str) -> bool {
    let (program, arguments): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(windows) {
        ("rundll32", vec!["url.dll,FileProtocolHandler", url])
    } else {
        ("xdg-open", vec![url])
    };
    std::process::Command::new(program)
        .args(arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests;
