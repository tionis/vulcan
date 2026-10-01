use super::*;
use crate::sync_forge::ForgeKind;
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const CLIENT: &str = "client-xyz";

#[derive(Default)]
struct ServerState {
    /// Challenge the "browser" captured from the authorization URL.
    challenge: Option<String>,
    redirect_uri: Option<String>,
    /// The refresh token the server currently honours.
    valid_refresh: Option<String>,
    issued: u32,
    refresh_calls: u32,
    token_requests: Vec<BTreeMap<String, String>>,
    redirect_to: Option<String>,
    refuse_refresh: bool,
    oversized: bool,
    expires_in: u64,
    /// Runs when a refresh is refused, to simulate another process winning.
    on_refresh_refused: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct FakeAuthServer {
    url: String,
    state: Arc<Mutex<ServerState>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FakeAuthServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn token_body(state: &mut ServerState) -> String {
    state.issued += 1;
    let refresh = format!("RT{}", state.issued);
    state.valid_refresh = Some(refresh.clone());
    serde_json::json!({
        "access_token": format!("AT{}", state.issued),
        "refresh_token": refresh,
        "token_type": "bearer",
        "expires_in": state.expires_in,
    })
    .to_string()
}

fn handle_token_request(
    state: &Arc<Mutex<ServerState>>,
    body: &str,
) -> (u16, String, Option<String>) {
    let mut state = state.lock().unwrap();
    let form = Url::parse(&format!("http://form.invalid/?{body}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect::<BTreeMap<_, _>>();
    state.token_requests.push(form.clone());
    if let Some(target) = state.redirect_to.clone() {
        return (302, String::new(), Some(target));
    }
    if state.oversized {
        return (200, "x".repeat(100 * 1024), None);
    }
    let get = |name: &str| form.get(name).map_or("", String::as_str);
    let refused = |description: &str| {
        (
            400,
            serde_json::json!({"error": "invalid_grant", "error_description": description})
                .to_string(),
            None,
        )
    };
    match get("grant_type") {
        "authorization_code" => {
            let verifier_ok = state.challenge.as_deref()
                == Some(
                    URL_SAFE_NO_PAD
                        .encode(Sha256::digest(get("code_verifier").as_bytes()))
                        .as_str(),
                );
            if get("code") != "good-code" {
                return refused("unknown or used code");
            }
            if !verifier_ok {
                return refused("PKCE verification failed");
            }
            if get("client_id") != CLIENT
                || Some(get("redirect_uri")) != state.redirect_uri.as_deref()
            {
                return refused("client or redirect mismatch");
            }
            (200, token_body(&mut state), None)
        }
        "refresh_token" => {
            state.refresh_calls += 1;
            if state.refuse_refresh || state.valid_refresh.as_deref() != Some(get("refresh_token"))
            {
                let hook = state.on_refresh_refused.clone();
                drop(state);
                if let Some(hook) = hook {
                    hook();
                }
                return refused("refresh token is no longer valid");
            }
            (200, token_body(&mut state), None)
        }
        _ => (
            400,
            r#"{"error":"unsupported_grant_type"}"#.to_owned(),
            None,
        ),
    }
}

fn serve() -> FakeAuthServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let state = Arc::new(Mutex::new(ServerState {
        expires_in: 3600,
        ..ServerState::default()
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let (thread_state, thread_stop) = (Arc::clone(&state), Arc::clone(&stop));
    let handle = std::thread::spawn(move || {
        while !thread_stop.load(Ordering::SeqCst) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            let body = loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                buffer.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&buffer).into_owned();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if buffer.len() >= end + 4 + length || read == 0 {
                        break text[end + 4..].to_owned();
                    }
                } else if read == 0 {
                    break String::new();
                }
            };
            let (status, payload, location) = handle_token_request(&thread_state, &body);
            let response = match location {
                Some(location) => format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                None => format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                ),
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    FakeAuthServer {
        url,
        state,
        stop,
        handle: Some(handle),
    }
}

fn config(server: &FakeAuthServer) -> ForgeConfig {
    ForgeConfig::new(
        ForgeKind::Forgejo,
        &server.url,
        "owner/vault",
        None,
        Some(CLIENT),
    )
    .unwrap()
}

fn store() -> (TempDir, TokenStore) {
    let dir = TempDir::new().unwrap();
    let store = TokenStore {
        directory: dir.path().join("forge-oauth"),
    };
    (dir, store)
}

/// What the user's browser does after approving: records the PKCE challenge
/// from the authorization URL and calls the loopback redirect.
fn browser<'a>(
    server: &'a FakeAuthServer,
    code: &'static str,
    tamper: fn(String) -> String,
) -> impl Fn(&str) + 'a {
    move |authorize: &str| {
        let url = Url::parse(authorize).unwrap();
        let params = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
        {
            let mut state = server.state.lock().unwrap();
            state.challenge = params.get("code_challenge").cloned();
            state.redirect_uri = params.get("redirect_uri").cloned();
        }
        let redirect = params["redirect_uri"].clone();
        let state_param = tamper(params["state"].clone());
        std::thread::spawn(move || {
            let client = reqwest::blocking::Client::new();
            let _ = client
                .get(format!("{redirect}?code={code}&state={state_param}"))
                .send();
        });
    }
}

fn plain(state: String) -> String {
    state
}

#[test]
fn login_runs_the_pkce_flow_and_stores_owner_only_tokens() {
    let server = serve();
    let (_dir, store) = store();
    let seen = Arc::new(Mutex::new(Url::parse("http://x").unwrap()));
    let capture = {
        let inner = browser(&server, "good-code", plain);
        let seen = Arc::clone(&seen);
        move |authorize: &str| {
            *seen.lock().unwrap() = Url::parse(authorize).unwrap();
            inner(authorize);
        }
    };

    let report =
        forge_login_with(&store, &config(&server), Duration::from_secs(10), &capture).unwrap();
    assert!(report.has_refresh_token);
    assert_eq!(report.expires_in_seconds, 3600);

    let params = seen
        .lock()
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect::<BTreeMap<_, _>>();
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["client_id"], CLIENT);
    assert!(params["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert!(params["state"].len() >= 40, "an unguessable state");
    assert!(seen
        .lock()
        .unwrap()
        .path()
        .ends_with("/login/oauth/authorize"));

    // The token request proved possession of the verifier and sent no secret.
    let state = server.state.lock().unwrap();
    let request = &state.token_requests[0];
    assert_eq!(request["grant_type"], "authorization_code");
    assert!(!request.contains_key("client_secret"));
    assert!((43..=128).contains(&request["code_verifier"].len()));
    drop(state);

    let origin = forge_origin(&config(&server)).unwrap();
    let saved = store.load(&origin, CLIENT).unwrap().expect("tokens saved");
    assert_eq!(
        (saved.access_token.as_str(), saved.refresh_token.as_deref()),
        ("AT1", Some("RT1"))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file_mode = fs::metadata(store.path(&origin, CLIENT))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = fs::metadata(&store.directory).unwrap().permissions().mode() & 0o777;
        assert_eq!((file_mode, dir_mode), (0o600, 0o700));
    }
}

#[test]
fn a_redirect_with_the_wrong_state_aborts_and_stores_nothing() {
    let server = serve();
    let (_dir, store) = store();
    let error = forge_login_with(
        &store,
        &config(&server),
        Duration::from_secs(10),
        &browser(&server, "good-code", |_| "forged-state-value".to_owned()),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("unexpected `state`"), "{error}");
    assert!(
        server.state.lock().unwrap().token_requests.is_empty(),
        "no code was exchanged"
    );
    assert!(store
        .load(&forge_origin(&config(&server)).unwrap(), CLIENT)
        .unwrap()
        .is_none());
}

#[test]
fn a_denied_login_reports_a_sanitized_reason() {
    let server = serve();
    let (_dir, store) = store();
    let opener = |authorize: &str| {
        let url = Url::parse(authorize).unwrap();
        let redirect = url
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .unwrap()
            .1
            .into_owned();
        std::thread::spawn(move || {
            let _ = reqwest::blocking::get(format!(
                "{redirect}?error=access_denied&error_description=user%0Adeclined%1B%5B31m"
            ));
        });
    };
    let error = forge_login_with(&store, &config(&server), Duration::from_secs(10), &opener)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("access_denied") && error.contains("declined"),
        "{error}"
    );
    assert!(
        !error.contains('\n') && !error.contains('\u{1b}'),
        "control characters are stripped"
    );
    assert!(store
        .load(&forge_origin(&config(&server)).unwrap(), CLIENT)
        .unwrap()
        .is_none());
}

#[test]
fn a_login_that_never_returns_times_out() {
    let server = serve();
    let (_dir, store) = store();
    let started = Instant::now();
    let error = forge_login_with(
        &store,
        &config(&server),
        Duration::from_millis(400),
        &|_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn stray_requests_do_not_abort_a_login_in_progress() {
    let server = serve();
    let (_dir, store) = store();
    let opener = |authorize: &str| {
        let url = Url::parse(authorize).unwrap();
        let params = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
        {
            let mut state = server.state.lock().unwrap();
            state.challenge = params.get("code_challenge").cloned();
            state.redirect_uri = params.get("redirect_uri").cloned();
        }
        let redirect = params["redirect_uri"].clone();
        let state_param = params["state"].clone();
        // Like a browser, never block the caller: a favicon fetch and a probe
        // arrive before the real redirect.
        std::thread::spawn(move || {
            let _ = reqwest::blocking::get(format!("{redirect}favicon.ico"));
            let _ = reqwest::blocking::get(redirect.clone());
            let _ =
                reqwest::blocking::get(format!("{redirect}?code=good-code&state={state_param}"));
        });
    };
    let started = Instant::now();
    assert!(forge_login_with(&store, &config(&server), Duration::from_secs(10), &opener).is_ok());
    assert!(started.elapsed() < Duration::from_secs(8));
}

#[test]
fn the_token_endpoint_can_refuse_a_code_and_nothing_is_stored() {
    let server = serve();
    let (_dir, store) = store();
    let error = forge_login_with(
        &store,
        &config(&server),
        Duration::from_secs(10),
        &browser(&server, "bad-code", plain),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("invalid_grant") && error.contains("unknown or used code"),
        "{error}"
    );
    assert!(store
        .load(&forge_origin(&config(&server)).unwrap(), CLIENT)
        .unwrap()
        .is_none());
}

#[test]
fn redirects_from_the_token_endpoint_are_never_followed() {
    let elsewhere = serve();
    let server = serve();
    server.state.lock().unwrap().redirect_to =
        Some(format!("{}/login/oauth/access_token", elsewhere.url));
    let (_dir, store) = store();
    let error = forge_login_with(
        &store,
        &config(&server),
        Duration::from_secs(10),
        &browser(&server, "good-code", plain),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("302") || error.contains("unexpected token response"),
        "{error}"
    );
    assert!(
        elsewhere.state.lock().unwrap().token_requests.is_empty(),
        "the redirect target was never contacted"
    );
}

#[test]
fn oversized_and_malformed_token_responses_are_rejected() {
    let server = serve();
    let client = http_client(Duration::from_secs(5)).unwrap();
    let origin = forge_origin(&config(&server)).unwrap();
    server.state.lock().unwrap().oversized = true;
    let error = request_tokens(
        &client,
        &config(&server),
        CLIENT,
        &origin,
        &[("grant_type", "x")],
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("size limit") || error.contains("unexpected"),
        "{error}"
    );
}

fn expired_tokens(origin: &str, refresh: Option<&str>) -> StoredTokens {
    StoredTokens {
        version: TOKEN_FILE_VERSION,
        origin: origin.to_owned(),
        client_id: CLIENT.to_owned(),
        access_token: "STALE".to_owned(),
        refresh_token: refresh.map(str::to_owned),
        expires_at_unix: 1,
        obtained_at_unix: 0,
    }
}

#[test]
fn a_credential_never_prints_its_token() {
    let credential = ForgeCredential {
        token: Zeroizing::new("super-secret-value".to_owned()),
        source: CredentialSource::Env,
    };
    let rendered = format!("{credential:?}");
    assert!(
        !rendered.contains("super-secret-value") && rendered.contains("redacted"),
        "{rendered}"
    );
}

fn no_env(_: &str) -> Option<String> {
    None
}

#[test]
fn a_valid_login_wins_over_the_environment_variable() {
    let server = serve();
    let (_dir, store) = store();
    let mut cfg = config(&server);
    cfg.token_env = Some("FORGE_TOKEN".to_owned());
    let origin = forge_origin(&cfg).unwrap();
    store
        .save(&StoredTokens {
            expires_at_unix: now_unix() + 3600,
            access_token: "GOOD".to_owned(),
            ..expired_tokens(&origin, Some("RT"))
        })
        .unwrap();

    let credential =
        resolve_with(&store, &cfg, &|_| Some("FROM-ENV".to_owned()), now_unix()).unwrap();
    assert_eq!(
        (credential.token.as_str(), credential.source),
        ("GOOD", CredentialSource::Oauth)
    );
    assert_eq!(
        server.state.lock().unwrap().refresh_calls,
        0,
        "no refresh while valid"
    );
}

#[test]
fn without_a_login_the_environment_variable_is_the_fallback() {
    let server = serve();
    let (_dir, store) = store();
    let mut cfg = config(&server);
    cfg.token_env = Some("FORGE_TOKEN".to_owned());
    let credential = resolve_with(
        &store,
        &cfg,
        &|name| (name == "FORGE_TOKEN").then(|| "  from-env\n".to_owned()),
        now_unix(),
    )
    .unwrap();
    assert_eq!(
        (credential.token.as_str(), credential.source),
        ("from-env", CredentialSource::Env)
    );

    let error = resolve_with(&store, &cfg, &no_env, now_unix())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("sync forge login") && error.contains("$FORGE_TOKEN"),
        "{error}"
    );
    let error = resolve_with(&store, &config(&server), &no_env, now_unix())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("sync forge login") && !error.contains('$'),
        "{error}"
    );
}

#[test]
fn an_expired_token_refreshes_once_and_rotates_the_refresh_token() {
    let server = serve();
    let (_dir, store) = store();
    let cfg = config(&server);
    let origin = forge_origin(&cfg).unwrap();
    {
        let mut state = server.state.lock().unwrap();
        state.valid_refresh = Some("RT0".to_owned());
    }
    store.save(&expired_tokens(&origin, Some("RT0"))).unwrap();

    let credential = resolve_with(&store, &cfg, &no_env, now_unix()).unwrap();
    assert_eq!(credential.token.as_str(), "AT1");
    let saved = store.load(&origin, CLIENT).unwrap().unwrap();
    assert_eq!(
        saved.refresh_token.as_deref(),
        Some("RT1"),
        "the rotated refresh token is kept"
    );

    let again = resolve_with(&store, &cfg, &no_env, now_unix()).unwrap();
    assert_eq!(again.token.as_str(), "AT1");
    assert_eq!(
        server.state.lock().unwrap().refresh_calls,
        1,
        "the fresh token is reused"
    );
}

#[test]
fn a_refresh_the_forge_refuses_falls_back_or_asks_for_a_new_login() {
    let server = serve();
    let (_dir, store) = store();
    let mut cfg = config(&server);
    let origin = forge_origin(&cfg).unwrap();
    server.state.lock().unwrap().refuse_refresh = true;
    store.save(&expired_tokens(&origin, Some("RT0"))).unwrap();

    let error = resolve_with(&store, &cfg, &no_env, now_unix())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("sync forge login") && error.contains("invalid_grant"),
        "{error}"
    );
    assert!(
        store.load(&origin, CLIENT).unwrap().is_some(),
        "a failed refresh never deletes the login"
    );

    cfg.token_env = Some("FORGE_TOKEN".to_owned());
    let credential =
        resolve_with(&store, &cfg, &|_| Some("env-token".to_owned()), now_unix()).unwrap();
    assert_eq!(credential.source, CredentialSource::Env);

    // No refresh token at all.
    store.save(&expired_tokens(&origin, None)).unwrap();
    let error = resolve_with(&store, &config(&server), &no_env, now_unix())
        .unwrap_err()
        .to_string();
    assert!(error.contains("no refresh token"), "{error}");
}

#[test]
fn losing_a_rotation_race_uses_the_winners_fresh_login() {
    let server = serve();
    let (dir, store) = store();
    let cfg = config(&server);
    let origin = forge_origin(&cfg).unwrap();
    server.state.lock().unwrap().valid_refresh = Some("RT-WINNER".to_owned());
    store
        .save(&expired_tokens(&origin, Some("RT-LOSER")))
        .unwrap();
    // While our refresh is being refused, another process saves a fresh login.
    let winner = TokenStore {
        directory: dir.path().join("forge-oauth"),
    };
    let fresh = StoredTokens {
        expires_at_unix: now_unix() + 3600,
        access_token: "WINNER-AT".to_owned(),
        ..expired_tokens(&origin, Some("RT-WINNER"))
    };
    server.state.lock().unwrap().on_refresh_refused = Some(Arc::new(move || {
        winner.save(&fresh).unwrap();
    }));

    let credential = resolve_with(&store, &cfg, &no_env, now_unix()).unwrap();
    assert_eq!(credential.token.as_str(), "WINNER-AT");
}

#[test]
fn stored_logins_are_validated_before_use() {
    let server = serve();
    let (_dir, store) = store();
    let cfg = config(&server);
    let origin = forge_origin(&cfg).unwrap();

    // A file for a different origin cannot be used for this forge.
    let other = StoredTokens {
        origin: "https://other.example".to_owned(),
        ..expired_tokens(&origin, None)
    };
    fs::create_dir_all(&store.directory).unwrap();
    fs::write(
        store.path(&origin, CLIENT),
        serde_json::to_vec(&other).unwrap(),
    )
    .unwrap();
    assert!(store
        .load(&origin, CLIENT)
        .unwrap_err()
        .to_string()
        .contains("does not match"));

    fs::write(
        store.path(&origin, CLIENT),
        br#"{"version":1,"unknown":true}"#,
    )
    .unwrap();
    assert!(store.load(&origin, CLIENT).is_err());
    fs::write(store.path(&origin, CLIENT), vec![b' '; 20_000]).unwrap();
    assert!(store.load(&origin, CLIENT).is_err());
}

#[test]
fn status_is_local_and_never_exposes_a_token() {
    let server = serve();
    let (_dir, store) = store();
    let cfg = config(&server);
    let origin = forge_origin(&cfg).unwrap();

    let none = status_with(&store, &cfg, 1_000).unwrap().unwrap();
    assert!(!none.logged_in && none.expires_in_seconds.is_none());
    assert!(status_with(
        &store,
        &ForgeConfig::new(ForgeKind::Forgejo, &server.url, "o/r", Some("T"), None).unwrap(),
        1
    )
    .unwrap()
    .is_none());

    store
        .save(&StoredTokens {
            expires_at_unix: 1_500,
            ..expired_tokens(&origin, Some("RT"))
        })
        .unwrap();
    let live = status_with(&store, &cfg, 1_000).unwrap().unwrap();
    assert_eq!(
        (
            live.logged_in,
            live.access_token_expired,
            live.expires_in_seconds
        ),
        (true, false, Some(500))
    );
    let expired = status_with(&store, &cfg, 2_000).unwrap().unwrap();
    assert!(expired.access_token_expired && expired.has_refresh_token);
    let rendered = serde_json::to_string(&live).unwrap();
    assert!(
        !rendered.contains("STALE") && !rendered.contains("RT"),
        "{rendered}"
    );

    assert!(store.remove(&origin, CLIENT).unwrap());
    assert!(!store.remove(&origin, CLIENT).unwrap());
}

#[test]
fn pkce_pairs_follow_rfc_7636() {
    let (verifier, challenge) = pkce_pair().unwrap();
    assert!((43..=128).contains(&verifier.len()));
    assert!(verifier
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    assert_eq!(
        challenge,
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    );
    assert_ne!(verifier, pkce_pair().unwrap().0);
    // RFC 7636 appendix B test vector.
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(
            b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        )),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn small_helpers_behave() {
    assert!(constant_time_eq("abc", "abc"));
    assert!(!constant_time_eq("abc", "abd") && !constant_time_eq("abc", "abcd"));
    assert_eq!(sanitize("a\nb\u{1b}c"), "abc");
    assert_eq!(sanitize(&"x".repeat(1000)).len(), MAX_MESSAGE_CHARS);
}

#[test]
fn endpoints_respect_a_forge_served_under_a_subpath() {
    let cfg = ForgeConfig::new(
        ForgeKind::Forgejo,
        "https://example.com/forge",
        "o/r",
        None,
        Some(CLIENT),
    )
    .unwrap();
    let url = authorize_url(&cfg, CLIENT, "http://127.0.0.1:9/", "s", "c").unwrap();
    assert_eq!(url.path(), "/forge/login/oauth/authorize");
    assert_eq!(forge_origin(&cfg).unwrap(), "https://example.com");
}
