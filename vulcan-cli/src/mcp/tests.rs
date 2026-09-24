use super::*;
use crate::McpToolPackModeArg;
use vulcan_core::{PermissionProfile, TasksQueryResult};

#[test]
fn restricted_mcp_read_reports_exclude_denied_task_paths() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir should create");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(paths.vulcan_dir()).expect("config directory should create");
    fs::write(
        paths.config_file(),
        "[permissions.profiles.public]\nread = { allow = [\"folder:Public/**\"] }\n",
    )
    .expect("config should write");
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("public")).expect("profile should resolve"),
    );
    let public = serde_json::json!({"path": "Public/Task.md", "text": "visible"});
    let private = serde_json::json!({"path": "Private/Task.md", "text": "secret"});
    let mut report = TasksQueryResult {
        tasks: vec![public.clone(), private.clone()],
        groups: vec![vulcan_core::TasksQueryGroup {
            field: "path".to_string(),
            key: Value::String("all".to_string()),
            tasks: vec![public, private],
        }],
        result_count: 2,
        hidden_fields: Vec::new(),
        shown_fields: Vec::new(),
        short_mode: false,
        plan: None,
    };

    mcp_read_tools::filter_tasks_query_report(&guard, &mut report);

    assert_eq!(report.result_count, 1);
    assert_eq!(report.tasks[0]["path"], "Public/Task.md");
    assert_eq!(report.groups[0].tasks.len(), 1);
}

#[test]
fn http_parser_rejects_oversized_content_length_before_body_read() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener.local_addr().expect("listener address");
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(address).expect("client should connect");
        write!(
            stream,
            "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\n\r\n",
            MAX_MCP_HTTP_BODY_BYTES + 1
        )
        .expect("request headers should write");
    });
    let (mut stream, _) = listener.accept().expect("server should accept");

    let error = read_mcp_http_request(&mut stream).expect_err("oversized body should fail");
    client.join().expect("client thread should finish");

    assert_eq!(error.status, 413);
    assert!(error.message.contains("exceeds maximum size"));
}

#[test]
fn http_sessions_reject_cross_subject_grant_remote_and_token_reuse() {
    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    let core = McpServerCore::new(
        &paths,
        Some("readonly"),
        &[McpToolPackArg::NotesRead],
        McpToolPackModeArg::Static,
    )
    .expect("MCP core");
    let instance = Ulid::new();
    let grant = Ulid::new();
    let authority = McpSessionAuthority::granted(
        vulcan_daemon::mcp_remote::McpRemoteId::parse("personal-chatgpt").expect("remote"),
        instance,
        grant,
        "client-a".to_string(),
        "https://identity.example.test/alice".to_string(),
        vulcan_daemon::registry::WikiId::parse("personal").expect("wiki"),
        "https://mcp.example.test/personal".to_string(),
        "readonly".to_string(),
        vec!["notes-read".to_string()],
        vec!["mcp:tools".to_string()],
        "alice-token",
    );
    let session = McpHttpSession::new(core, authority.clone());
    assert!(session.authority.matches(&authority));

    let bob_grant = McpSessionAuthority::granted(
        vulcan_daemon::mcp_remote::McpRemoteId::parse("personal-chatgpt").expect("remote"),
        instance,
        Ulid::new(),
        "client-b".to_string(),
        "https://identity.example.test/bob".to_string(),
        vulcan_daemon::registry::WikiId::parse("personal").expect("wiki"),
        "https://mcp.example.test/personal".to_string(),
        "readonly".to_string(),
        vec!["notes-read".to_string()],
        vec!["mcp:tools".to_string()],
        "bob-token",
    );
    assert!(!session.authority.matches(&bob_grant));

    let other_remote = McpSessionAuthority::granted(
        vulcan_daemon::mcp_remote::McpRemoteId::parse("work-chatgpt").expect("remote"),
        Ulid::new(),
        grant,
        "client-a".to_string(),
        "https://identity.example.test/alice".to_string(),
        vulcan_daemon::registry::WikiId::parse("work").expect("wiki"),
        "https://mcp.example.test/work".to_string(),
        "readonly".to_string(),
        vec!["notes-read".to_string()],
        vec!["mcp:tools".to_string()],
        "alice-token",
    );
    assert!(!session.authority.matches(&other_remote));

    let replacement_token = McpSessionAuthority::granted(
        vulcan_daemon::mcp_remote::McpRemoteId::parse("personal-chatgpt").expect("remote"),
        instance,
        grant,
        "client-a".to_string(),
        "https://identity.example.test/alice".to_string(),
        vulcan_daemon::registry::WikiId::parse("personal").expect("wiki"),
        "https://mcp.example.test/personal".to_string(),
        "readonly".to_string(),
        vec!["notes-read".to_string()],
        vec!["mcp:tools".to_string()],
        "replacement-token",
    );
    assert!(!session.authority.matches(&replacement_token));
}

#[cfg(feature = "oauth")]
fn oauth_options() -> McpHttpOptions {
    McpHttpOptions {
        bind: "127.0.0.1:8765".to_string(),
        endpoint: "/mcp".to_string(),
        auth_token: None,
        public_url: Some("https://wiki.example.test/mcp".to_string()),
        oauth_issuer: Some("https://auth.example.test/application/o/vulcan/".to_string()),
        oauth_audience: vec!["vulcan-mcp".to_string()],
        oauth_jwks_url: Some("https://auth.example.test/application/o/vulcan/jwks/".to_string()),
        oauth_allowed_sub: vec!["user-id".to_string()],
        oauth_allowed_email: Vec::new(),
        oauth_local_client_id: None,
        oauth_local_redirect_uri: Vec::new(),
        oauth_local_client_secret: None,
        oauth_local_approval_token: None,
        oauth_local_subject: Some("local-user".to_string()),
        oauth_local_email: None,
        oauth_dcr: false,
        oauth_dcr_allowed_redirect_host: Vec::new(),
        oauth_indieauth_authorization_endpoint: None,
        oauth_indieauth_token_endpoint: None,
        oauth_indieauth_client_id: None,
        oauth_indieauth_redirect_uri: None,
        oauth_indieauth_me: None,
        oauth_local_user: Vec::new(),
        instance_id: None,
        oauth_storage_dir: None,
        request_timeout: DEFAULT_MCP_REQUEST_TIMEOUT,
    }
}

#[cfg(feature = "oauth")]
#[test]
fn mcp_http_listener_reports_bound_address_and_stops_on_supervisor_signal() {
    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    let mut options = oauth_options();
    options.bind = "127.0.0.1:0".to_string();
    options.public_url = None;
    options.oauth_issuer = None;
    options.oauth_audience.clear();
    options.oauth_jwks_url = None;
    options.oauth_allowed_sub.clear();
    options.oauth_local_subject = None;
    let stop = Arc::new(ShutdownSignal::new(false));
    let runner_stop = Arc::clone(&stop);
    let (ready_sender, ready_receiver) = mpsc::channel();
    let (done_sender, done_receiver) = mpsc::channel();
    let runner = thread::spawn(move || {
        let on_ready = |address| ready_sender.send(address).map_err(CliError::operation);
        let result = run_mcp_http_server_inner(
            &paths,
            None,
            &[],
            McpToolPackModeArg::Static,
            &options,
            None,
            McpHttpLifecycle {
                stop: Some(&runner_stop),
                ready: Some(&on_ready),
            },
        );
        done_sender.send(result).expect("completion receiver");
    });
    let address = ready_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("listener readiness after bind");
    TcpStream::connect(address).expect("ready listener should accept TCP connections");
    stop.cancel();
    done_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("listener should stop promptly")
        .expect("listener should stop cleanly");
    runner.join().expect("listener thread");
}

#[cfg(feature = "oauth")]
#[test]
fn static_local_oauth_requires_registered_safe_redirects() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let mut options = oauth_options();
    options.oauth_issuer = None;
    options.oauth_audience.clear();
    options.oauth_jwks_url = None;
    options.oauth_allowed_sub.clear();
    options.oauth_local_client_id = Some("client".to_string());
    options.oauth_local_client_secret = Some("client-secret".to_string());
    options.oauth_local_approval_token = Some("approval".to_string());

    let error = build_mcp_oauth_validator(&paths, None, &options)
        .expect_err("missing redirect registration must fail");
    assert!(error.to_string().contains("--oauth-local-redirect-uri"));

    options.oauth_local_redirect_uri = vec!["https://client.example/callback".to_string()];
    assert!(build_mcp_oauth_validator(&paths, None, &options).is_ok());
    assert!(valid_oauth_redirect_uri("https://client.example/callback"));
    assert!(!valid_oauth_redirect_uri(
        "https://client.example/callback\r\nX-Injected: yes"
    ));
    assert!(!valid_oauth_redirect_uri("http://client.example/callback"));
}

#[cfg(feature = "oauth")]
#[test]
fn client_id_metadata_documents_require_exact_public_client_metadata() {
    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    let issuer = Arc::new(
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://mcp.example.test/personal".to_string(),
            client_id: "static-client".to_string(),
            client_secret: "secret".to_string(),
            signing_key: "signing-key".to_string(),
            approval_token: String::new(),
            subject: "https://identity.example.test/alice".to_string(),
            email: None,
            users: Vec::new(),
            dcr_enabled: true,
        })
        .expect("issuer"),
    );
    let context = consent_test_context(&paths, issuer);
    let client_id = "https://client.example.test/oauth/client.json";
    let metadata = ClientIdMetadataDocument {
        client_id: client_id.to_string(),
        redirect_uris: vec!["https://client.example.test/callback".to_string()],
        token_endpoint_auth_method: "none".to_string(),
    };
    assert!(validate_client_id_metadata(
        &context,
        client_id,
        Some("https://client.example.test/callback"),
        &metadata,
    ));
    let mismatched = ClientIdMetadataDocument {
        client_id: "https://attacker.example.test/client.json".to_string(),
        ..metadata.clone()
    };
    assert!(!validate_client_id_metadata(
        &context,
        client_id,
        Some("https://client.example.test/callback"),
        &mismatched,
    ));
    let confidential = ClientIdMetadataDocument {
        token_endpoint_auth_method: "client_secret_post".to_string(),
        ..metadata
    };
    assert!(!validate_client_id_metadata(
        &context,
        client_id,
        Some("https://client.example.test/callback"),
        &confidential,
    ));
}

#[cfg(all(feature = "oauth", unix))]
#[test]
fn oauth_client_registry_is_atomic_owner_only_and_rejects_loose_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    let issuer = Arc::new(
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://mcp.example.test/personal".to_string(),
            client_id: "static-client".to_string(),
            client_secret: "secret".to_string(),
            signing_key: "signing-key".to_string(),
            approval_token: String::new(),
            subject: "https://identity.example.test/alice".to_string(),
            email: None,
            users: Vec::new(),
            dcr_enabled: true,
        })
        .expect("issuer"),
    );
    let mut context = consent_test_context(&paths, issuer);
    let registry = temporary.path().join("state/oauth-clients.json");
    context.oauth_clients_path = Some(registry.clone());
    context.oauth_clients.lock().expect("clients").insert(
        "client".to_string(),
        LocalOAuthRegisteredClient {
            client_id: "client".to_string(),
            client_secret: "secret-value".to_string(),
            redirect_uris: vec!["https://client.example.test/callback".to_string()],
            client_name: None,
            token_endpoint_auth_method: "client_secret_post".to_string(),
            client_id_issued_at: 1,
        },
    );
    save_oauth_registered_clients(&context).expect("save registry");
    assert_eq!(
        fs::metadata(&registry)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(load_oauth_registered_clients(&registry).is_ok());
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o644)).expect("loosen mode");
    assert!(load_oauth_registered_clients(&registry).is_err());
}

#[cfg(feature = "oauth")]
#[test]
fn protected_resource_metadata_path_accepts_root_and_endpoint_forms() {
    assert!(is_protected_resource_metadata_path(
        "/.well-known/oauth-protected-resource",
        "/mcp"
    ));
    assert!(is_protected_resource_metadata_path(
        "/.well-known/oauth-protected-resource/mcp",
        "/mcp"
    ));
    assert!(!is_protected_resource_metadata_path(
        "/.well-known/oauth-authorization-server",
        "/mcp"
    ));
}

#[cfg(feature = "oauth")]
#[test]
fn authorization_server_metadata_path_accepts_root_endpoint_and_oidc_forms() {
    assert!(is_authorization_server_metadata_path(
        "/.well-known/oauth-authorization-server",
        "/mcp"
    ));
    assert!(is_authorization_server_metadata_path(
        "/.well-known/oauth-authorization-server/mcp",
        "/mcp"
    ));
    assert!(is_authorization_server_metadata_path(
        "/.well-known/openid-configuration",
        "/mcp"
    ));
    assert!(is_authorization_server_metadata_path(
        "/.well-known/openid-configuration/mcp",
        "/mcp"
    ));
}

#[cfg(feature = "oauth")]
#[test]
fn oauth_scope_parser_defaults_deduplicates_and_rejects_widening() {
    assert_eq!(
        parse_mcp_oauth_scopes(None).expect("default scopes"),
        ["mcp:prompts", "mcp:resources", "mcp:tools"]
    );
    assert_eq!(
        parse_mcp_oauth_scopes(Some("mcp:tools mcp:resources mcp:tools"))
            .expect("supported scopes"),
        ["mcp:resources", "mcp:tools"]
    );
    let error = parse_mcp_oauth_scopes(Some("mcp:tools vault:admin"))
        .expect_err("unsupported scope must fail");
    assert_eq!(error.status, 400);
    assert!(String::from_utf8(error.body)
        .expect("JSON")
        .contains("invalid_scope"));
}

#[cfg(feature = "oauth")]
#[test]
fn local_oauth_user_bindings_parse_profile_and_email() {
    let users = parse_local_oauth_users(&[
        "https://tionis.dev/=daily-wiki-agent,eric@example.test".to_string(),
        "guest=readonly".to_string(),
    ])
    .unwrap();
    assert_eq!(users[0].subject, "https://tionis.dev/");
    assert_eq!(
        users[0].permission_profile.as_deref(),
        Some("daily-wiki-agent")
    );
    assert_eq!(users[0].email.as_deref(), Some("eric@example.test"));
    assert_eq!(users[1].subject, "guest");
    assert_eq!(users[1].permission_profile.as_deref(), Some("readonly"));
    assert!(parse_local_oauth_users(&["missing-profile".to_string()]).is_err());
}

#[cfg(feature = "oauth")]
#[test]
fn oauth_options_reject_shared_token_and_plain_http_public_url() {
    let paths = VaultPaths::new(".");
    let mut with_shared_token = oauth_options();
    with_shared_token.auth_token = Some("secret".to_string());
    assert!(build_mcp_oauth_validator(&paths, None, &with_shared_token)
        .unwrap_err()
        .to_string()
        .contains("mutually exclusive"));

    let mut plain_http = oauth_options();
    plain_http.public_url = Some("http://wiki.example.test/mcp".to_string());
    assert!(build_mcp_oauth_validator(&paths, None, &plain_http)
        .unwrap_err()
        .to_string()
        .contains("HTTPS"));
}

#[cfg(feature = "oauth")]
#[test]
fn oauth_options_require_audience_and_allowed_principal() {
    let paths = VaultPaths::new(".");
    let mut missing_audience = oauth_options();
    missing_audience.oauth_audience.clear();
    assert!(build_mcp_oauth_validator(&paths, None, &missing_audience)
        .unwrap_err()
        .to_string()
        .contains("--oauth-audience"));

    let mut missing_principal = oauth_options();
    missing_principal.oauth_allowed_sub.clear();
    missing_principal.oauth_allowed_email.clear();
    assert!(build_mcp_oauth_validator(&paths, None, &missing_principal)
        .unwrap_err()
        .to_string()
        .contains("--oauth-allowed-sub"));
}

#[cfg(feature = "oauth")]
#[test]
fn local_oauth_dcr_generates_and_reuses_issuer_secret() {
    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let paths = VaultPaths::new(tmp.path());
    let mut options = oauth_options();
    options.oauth_issuer = None;
    options.oauth_audience.clear();
    options.oauth_jwks_url = None;
    options.oauth_allowed_sub.clear();
    options.oauth_dcr = true;
    options.oauth_indieauth_me = Some("https://example.test/".to_string());

    assert!(
        build_mcp_oauth_validator(&paths, Some("readonly"), &options)
            .expect("DCR local issuer should initialize")
            .is_some()
    );
    let secret_path = oauth_issuer_secret_path(&paths, &options);
    let first_secret = fs::read_to_string(&secret_path).expect("issuer secret should be persisted");
    let signing_key_path = oauth_signing_key_path(&paths, &options);
    let first_signing_key =
        fs::read_to_string(&signing_key_path).expect("signing key should be persisted");
    assert!(!first_secret.trim().is_empty());
    assert!(!first_signing_key.trim().is_empty());
    assert_ne!(first_secret, first_signing_key);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&secret_path)
            .expect("issuer secret metadata should be readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let signing_mode = fs::metadata(&signing_key_path)
            .expect("signing key metadata should be readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(signing_mode, 0o600);
    }

    assert!(
        build_mcp_oauth_validator(&paths, Some("readonly"), &options)
            .expect("DCR local issuer should reuse persisted secret")
            .is_some()
    );
    let second_secret = fs::read_to_string(&secret_path).expect("issuer secret should still exist");
    let second_signing_key =
        fs::read_to_string(&signing_key_path).expect("signing key should still exist");
    assert_eq!(first_secret, second_secret);
    assert_eq!(first_signing_key, second_signing_key);
}

#[cfg(feature = "oauth")]
#[test]
fn single_user_indieauth_allows_me_with_process_permissions() {
    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let paths = VaultPaths::new(tmp.path());
    let mut options = oauth_options();
    options.oauth_issuer = None;
    options.oauth_audience.clear();
    options.oauth_jwks_url = None;
    options.oauth_allowed_sub.clear();
    options.oauth_local_subject = None;
    options.oauth_dcr = true;
    options.oauth_indieauth_me = Some("https://example.test/".to_string());

    let mode = build_mcp_oauth_validator(&paths, Some("readonly"), &options)
        .expect("single-user IndieAuth should initialize")
        .expect("local OAuth should be enabled");
    let McpOAuthMode::Local(issuer) = mode else {
        panic!("expected local OAuth issuer");
    };

    let user = issuer
        .user_for_subject("https://example.test")
        .expect("configured IndieAuth identity should be allowed");
    assert_eq!(user.subject, "https://example.test/");
    assert_eq!(user.permission_profile, None);
}

#[cfg(feature = "oauth")]
#[test]
fn single_user_indieauth_requires_permissions_or_explicit_binding() {
    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let paths = VaultPaths::new(tmp.path());
    let mut options = oauth_options();
    options.oauth_issuer = None;
    options.oauth_audience.clear();
    options.oauth_jwks_url = None;
    options.oauth_allowed_sub.clear();
    options.oauth_local_subject = None;
    options.oauth_dcr = true;
    options.oauth_indieauth_me = Some("https://example.test/".to_string());

    let error = build_mcp_oauth_validator(&paths, None, &options)
        .expect_err("implicit IndieAuth user without permissions must fail");
    assert!(error.to_string().contains("--permissions <profile>"));
    assert!(error.to_string().contains("--oauth-local-user"));
}

#[cfg(feature = "oauth")]
#[test]
fn explicit_indieauth_users_disable_the_single_user_default() {
    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let paths = VaultPaths::new(tmp.path());
    let mut options = oauth_options();
    options.oauth_issuer = None;
    options.oauth_audience.clear();
    options.oauth_jwks_url = None;
    options.oauth_allowed_sub.clear();
    options.oauth_local_subject = None;
    options.oauth_dcr = true;
    options.oauth_indieauth_me = Some("https://owner.example/".to_string());
    options.oauth_local_user = vec!["https://guest.example/=readonly".to_string()];

    let mode = build_mcp_oauth_validator(&paths, Some("daily-wiki-agent"), &options)
        .expect("multi-user IndieAuth should initialize")
        .expect("local OAuth should be enabled");
    let McpOAuthMode::Local(issuer) = mode else {
        panic!("expected local OAuth issuer");
    };

    assert!(issuer.user_for_subject("https://owner.example/").is_none());
    assert!(issuer.user_for_subject("https://guest.example/").is_some());
}

#[cfg(feature = "oauth")]
#[test]
fn indieauth_subject_mismatch_error_is_actionable() {
    let response = indieauth_subject_not_allowed_response("https://other.example/");
    let body = String::from_utf8(response.body).expect("response should be UTF-8");

    assert_eq!(response.status, 403);
    assert!(body.contains("https://other.example/"));
    assert!(body.contains("--oauth-indieauth-me"));
    assert!(body.contains("--permissions <profile>"));
    assert!(body.contains("--oauth-local-user"));
}

#[cfg(feature = "oauth")]
#[test]
fn indieauth_redirect_includes_pkce_challenge() {
    let indieauth = LocalOAuthIndieAuthConfig {
        authorization_endpoint: "https://indieauth.example.test/authorize".to_string(),
        token_endpoint: "https://indieauth.example.test/token".to_string(),
        client_id: "https://wiki.example.test".to_string(),
        redirect_uri: "https://wiki.example.test/oauth/indieauth/callback".to_string(),
        me: Some("https://example.test/".to_string()),
    };
    let response = local_oauth_redirect_to_indieauth(&indieauth, "state-value", "challenge-value");
    let location = response
        .extra_headers
        .iter()
        .find_map(|(name, value)| (name == "Location").then_some(value.as_str()))
        .expect("redirect location should be set");
    assert!(location.contains("code_challenge=challenge-value"));
    assert!(location.contains("code_challenge_method=S256"));
}

#[cfg(feature = "oauth")]
fn consent_test_context(paths: &VaultPaths, issuer: Arc<LocalOAuthIssuer>) -> McpHttpServerContext {
    McpHttpServerContext {
        paths: paths.clone(),
        requested_profile: Some("readonly".to_string()),
        tool_pack_args: vec![McpToolPackArg::NotesRead, McpToolPackArg::Search],
        tool_pack_mode_arg: McpToolPackModeArg::Static,
        endpoint: "/mcp".to_string(),
        auth_token: None,
        oauth: Some(McpOAuthMode::Local(issuer)),
        bind_addr: "127.0.0.1:8765".parse().expect("bind"),
        instance_id: Ulid::new(),
        sessions: Arc::new(Mutex::new(BTreeMap::new())),
        oauth_codes: Arc::new(Mutex::new(BTreeMap::new())),
        oauth_clients: Arc::new(Mutex::new(BTreeMap::new())),
        oauth_pending_indieauth: Arc::new(Mutex::new(BTreeMap::new())),
        oauth_pending_consent: Arc::new(Mutex::new(BTreeMap::new())),
        oauth_dcr_enabled: true,
        oauth_dcr_allowed_redirect_hosts: vec!["client.example.test".to_string()],
        oauth_local_redirect_uris: Vec::new(),
        oauth_indieauth: None,
        oauth_clients_path: None,
        named_runtime: None,
        request_timeout: DEFAULT_MCP_REQUEST_TIMEOUT,
    }
}

#[cfg(feature = "oauth")]
#[test]
#[allow(clippy::too_many_lines)]
fn indieauth_consent_requires_csrf_and_preserves_state_and_pkce() {
    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    let issuer = Arc::new(
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://mcp.example.test/personal".to_string(),
            client_id: "static-client".to_string(),
            client_secret: "client-secret".to_string(),
            signing_key: "distinct-signing-key".to_string(),
            approval_token: String::new(),
            subject: "https://identity.example.test/alice".to_string(),
            email: None,
            users: Vec::new(),
            dcr_enabled: true,
        })
        .expect("issuer"),
    );
    let context = consent_test_context(&paths, Arc::clone(&issuer));
    let transaction = "consent-transaction";
    context
        .oauth_pending_consent
        .lock()
        .expect("consent lock")
        .insert(
            transaction.to_string(),
            LocalOAuthPendingConsent {
                client_id: "client-a".to_string(),
                redirect_uri: "https://client.example.test/callback".to_string(),
                code_challenge: "original-pkce-challenge".to_string(),
                subject: "https://identity.example.test/alice".to_string(),
                scopes: vec!["mcp:tools".to_string()],
                resource: "https://mcp.example.test/personal".to_string(),
                state: Some("original-client-state".to_string()),
                csrf_token: "csrf-secret".to_string(),
                expires_at: std::time::Instant::now() + Duration::from_secs(60),
            },
        );

    let form = local_oauth_consent_form(
        &context,
        &issuer,
        transaction,
        &context
            .oauth_pending_consent
            .lock()
            .expect("consent lock")
            .get(transaction)
            .expect("pending")
            .clone(),
    );
    let html = String::from_utf8(form.body).expect("HTML");
    assert!(html.contains("client-a"));
    assert!(html.contains("https://identity.example.test/alice"));
    assert!(html.contains("readonly"));
    assert!(html.contains("notes-read, search"));
    assert!(form
        .extra_headers
        .iter()
        .any(|(name, value)| name == "Content-Security-Policy" && value.contains("form-action")));

    let invalid = McpHttpRequest {
        method: "POST".to_string(),
        path: "/oauth/consent".to_string(),
        query: String::new(),
        headers: BTreeMap::new(),
        body: b"transaction=consent-transaction&csrf_token=wrong&decision=approve".to_vec(),
    };
    assert_eq!(
        handle_local_oauth_consent(&context, &issuer, &invalid).status,
        403
    );
    assert!(context
        .oauth_pending_consent
        .lock()
        .expect("consent lock")
        .contains_key(transaction));

    let approve = McpHttpRequest {
        method: "POST".to_string(),
        path: "/oauth/consent".to_string(),
        query: String::new(),
        headers: BTreeMap::new(),
        body: b"transaction=consent-transaction&csrf_token=csrf-secret&decision=approve".to_vec(),
    };
    let response = handle_local_oauth_consent(&context, &issuer, &approve);
    assert_eq!(response.status, 302);
    let location = response
        .extra_headers
        .iter()
        .find_map(|(name, value)| (name == "Location").then_some(value))
        .expect("redirect");
    assert!(location.starts_with("https://client.example.test/callback?code="));
    assert!(location.ends_with("&state=original-client-state"));
    let codes = context.oauth_codes.lock().expect("codes lock");
    assert_eq!(codes.len(), 1);
    assert_eq!(
        codes.values().next().expect("code").code_challenge,
        "original-pkce-challenge"
    );
    drop(codes);
    assert!(context
        .oauth_pending_consent
        .lock()
        .expect("consent lock")
        .is_empty());

    context
        .oauth_pending_consent
        .lock()
        .expect("consent lock")
        .insert(
            "denied-transaction".to_string(),
            LocalOAuthPendingConsent {
                client_id: "client-a".to_string(),
                redirect_uri: "https://client.example.test/callback".to_string(),
                code_challenge: "unused-challenge".to_string(),
                subject: "https://identity.example.test/alice".to_string(),
                scopes: vec!["mcp:tools".to_string()],
                resource: "https://mcp.example.test/personal".to_string(),
                state: Some("denied-state".to_string()),
                csrf_token: "deny-csrf".to_string(),
                expires_at: std::time::Instant::now() + Duration::from_secs(60),
            },
        );
    let deny = McpHttpRequest {
        method: "POST".to_string(),
        path: "/oauth/consent".to_string(),
        query: String::new(),
        headers: BTreeMap::new(),
        body: b"transaction=denied-transaction&csrf_token=deny-csrf&decision=deny".to_vec(),
    };
    let denied = handle_local_oauth_consent(&context, &issuer, &deny);
    let denied_location = denied
        .extra_headers
        .iter()
        .find_map(|(name, value)| (name == "Location").then_some(value))
        .expect("denial redirect");
    assert!(denied_location.contains("error=access_denied"));
    assert!(denied_location.ends_with("&state=denied-state"));
    assert_eq!(context.oauth_codes.lock().expect("codes lock").len(), 1);
}

#[cfg(feature = "oauth")]
#[test]
#[allow(clippy::too_many_lines)]
fn named_consent_persists_and_enforces_a_revocable_grant() {
    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    let issuer = Arc::new(
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://mcp.example.test/personal".to_string(),
            client_id: "static-client".to_string(),
            client_secret: "client-secret".to_string(),
            signing_key: "distinct-signing-key".to_string(),
            approval_token: String::new(),
            subject: "https://identity.example.test/alice".to_string(),
            email: None,
            users: Vec::new(),
            dcr_enabled: true,
        })
        .expect("issuer"),
    );
    let mut context = consent_test_context(&paths, Arc::clone(&issuer));
    let store = McpAuthorizationStore::at(temporary.path().join("state"));
    context.named_runtime = Some(NamedMcpRuntime {
        remote_id: vulcan_daemon::mcp_remote::McpRemoteId::parse("personal-chatgpt")
            .expect("remote"),
        wiki_id: vulcan_daemon::registry::WikiId::parse("personal").expect("wiki"),
        ceiling_profile: "readonly".to_string(),
        default_profile: "readonly".to_string(),
        eligible_tool_packs: vec!["notes-read".to_string(), "search".to_string()],
        authorization_store: store.clone(),
    });
    let verifier = "named-consent-pkce-verifier";
    context
        .oauth_pending_consent
        .lock()
        .expect("consent lock")
        .insert(
            "named-transaction".to_string(),
            LocalOAuthPendingConsent {
                client_id: "static-client".to_string(),
                redirect_uri: "https://client.example.test/callback".to_string(),
                code_challenge: pkce_s256_challenge(verifier),
                subject: "https://identity.example.test/alice".to_string(),
                scopes: vec!["mcp:tools".to_string()],
                resource: "https://mcp.example.test/personal".to_string(),
                state: Some("client-state".to_string()),
                csrf_token: "csrf-secret".to_string(),
                expires_at: std::time::Instant::now() + Duration::from_secs(60),
            },
        );
    let approval = McpHttpRequest {
        method: "POST".to_string(),
        path: "/oauth/consent".to_string(),
        query: String::new(),
        headers: BTreeMap::new(),
        body: b"transaction=named-transaction&csrf_token=csrf-secret&decision=approve&permission_profile=readonly&pack_notes-read=on&expiry_days=7".to_vec(),
    };
    assert_eq!(
        handle_local_oauth_consent(&context, &issuer, &approval).status,
        302
    );
    let grants = store.list_grants(None).expect("durable grants");
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].tool_packs, ["notes-read"]);
    let code = context
        .oauth_codes
        .lock()
        .expect("codes")
        .keys()
        .next()
        .expect("code")
        .clone();
    let token_request = McpHttpRequest {
        method: "POST".to_string(),
        path: "/oauth/token".to_string(),
        query: String::new(),
        headers: BTreeMap::new(),
        body: format!(
            "grant_type=authorization_code&client_id=static-client&client_secret=client-secret&code={code}&code_verifier={verifier}&redirect_uri=https%3A%2F%2Fclient.example.test%2Fcallback"
        )
        .into_bytes(),
    };
    let token_response = handle_local_oauth_token(&context, &issuer, &token_request);
    assert_eq!(token_response.status, 200);
    let tokens: Value = serde_json::from_slice(&token_response.body).expect("token JSON");
    let refresh_token = tokens["refresh_token"]
        .as_str()
        .expect("refresh token")
        .to_string();
    let access_token = tokens["access_token"].as_str().expect("access token");
    let request = McpHttpRequest {
        method: "POST".to_string(),
        path: "/mcp".to_string(),
        query: String::new(),
        headers: BTreeMap::from([(
            "authorization".to_string(),
            format!("Bearer {access_token}"),
        )]),
        body: Vec::new(),
    };
    let authority = authenticate_mcp_http_request(&context, &request).expect("grant authority");
    assert_eq!(authority.grant_id, Some(grants[0].id));
    assert_eq!(authority.tool_packs, ["notes-read"]);

    let refresh = |token: &str| {
        McpHttpRequest {
        method: "POST".to_string(),
        path: "/oauth/token".to_string(),
        query: String::new(),
        headers: BTreeMap::new(),
        body: format!(
            "grant_type=refresh_token&client_id=static-client&client_secret=client-secret&refresh_token={token}&resource=https%3A%2F%2Fmcp.example.test%2Fpersonal&scope=mcp%3Atools"
        )
        .into_bytes(),
    }
    };
    let rotated = handle_local_oauth_token(&context, &issuer, &refresh(&refresh_token));
    assert_eq!(rotated.status, 200);
    let rotated: Value = serde_json::from_slice(&rotated.body).expect("rotated token JSON");
    let replacement = rotated["refresh_token"]
        .as_str()
        .expect("replacement refresh token")
        .to_string();
    assert_ne!(replacement, refresh_token);
    assert_eq!(
        handle_local_oauth_token(&context, &issuer, &refresh(&refresh_token)).status,
        400,
        "refresh replay must be rejected and revoke the family"
    );
    assert_eq!(
        handle_local_oauth_token(&context, &issuer, &refresh(&replacement)).status,
        400,
        "the replacement must be invalid after replay revocation"
    );

    store
        .revoke_grant(grants[0].id, current_unix_timestamp(), false)
        .expect("revoke grant");
    assert!(authenticate_mcp_http_request(&context, &request).is_err());
}

#[cfg(feature = "oauth")]
#[test]
fn named_runtime_locks_conflict_per_remote_but_not_across_remotes() {
    let temporary = tempfile::tempdir().expect("temporary state");
    let definition = |name: &str, instance_id: Ulid| McpRemoteDefinition {
        version: vulcan_daemon::mcp_remote::MCP_REMOTE_DEFINITION_VERSION,
        id: vulcan_daemon::mcp_remote::McpRemoteId::parse(name).expect("remote"),
        instance_id,
        bind: "127.0.0.1:8765".to_string(),
        public_url: format!("https://mcp.example.test/{name}"),
        authentication: McpRemoteAuthentication::IndieAuth {
            identity: "https://identity.example.test/alice".to_string(),
        },
        vaults: vec![vulcan_daemon::mcp_remote::McpRemoteVault {
            wiki_id: vulcan_daemon::registry::WikiId::parse("personal").expect("wiki"),
            ceiling_profile: "readonly".to_string(),
            default_profile: "readonly".to_string(),
            tool_packs: vec!["notes-read".to_string()],
        }],
    };
    let first = definition("first", Ulid::new());
    let second = definition("second", Ulid::new());
    let first_dir = temporary.path().join("first");
    let _first_lock = acquire_named_remote_runtime_lock(&first_dir, &first).expect("first lock");
    assert!(acquire_named_remote_runtime_lock(&first_dir, &first).is_err());
    assert!(acquire_named_remote_runtime_lock(&temporary.path().join("second"), &second).is_ok());
}

#[cfg(feature = "oauth")]
#[test]
fn resident_mcp_service_groups_instances_and_rejects_unsupported_multi_vault_routing() {
    let temporary = tempfile::tempdir().expect("temporary state");
    let process = DaemonProcessContext {
        registry: vulcan_daemon::registry::WikiRegistry::at(temporary.path().join("daemon.toml")),
        state_root: temporary.path().join("state"),
        verbose: false,
    };
    let definition = |name: &str| McpRemoteDefinition {
        version: vulcan_daemon::mcp_remote::MCP_REMOTE_DEFINITION_VERSION,
        id: vulcan_daemon::mcp_remote::McpRemoteId::parse(name).expect("remote ID"),
        instance_id: Ulid::new(),
        bind: "127.0.0.1:8765".to_string(),
        public_url: format!("https://mcp.example.test/{name}"),
        authentication: McpRemoteAuthentication::IndieAuth {
            identity: "https://identity.example.test/alice".to_string(),
        },
        vaults: vec![vulcan_daemon::mcp_remote::McpRemoteVault {
            wiki_id: vulcan_daemon::registry::WikiId::parse("personal").expect("wiki ID"),
            ceiling_profile: "readonly".to_string(),
            default_profile: "readonly".to_string(),
            tool_packs: vec!["notes-read".to_string()],
        }],
    };
    assert!(resident_named_mcp_service(&process, &[])
        .expect("empty registry")
        .is_none());
    let first = definition("first");
    let second = definition("second");
    let service = resident_named_mcp_service(&process, &[first.clone(), second])
        .expect("two remotes share one supervised service")
        .expect("service");
    assert_eq!(service.definition.id.as_str(), "listener.mcp-remotes");
    assert!(service.definition.required);

    let mut unsupported = first;
    unsupported.vaults.push(unsupported.vaults[0].clone());
    let error = resident_named_mcp_service(&process, &[unsupported])
        .expect_err("multi-vault routing must not be silently narrowed");
    assert!(error
        .message
        .contains("multi-vault routing is not yet available"));
}

#[test]
fn mcp_tool_calls_return_structured_timeout_errors() {
    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let paths = VaultPaths::new(tmp.path());
    vulcan_core::initialize_vulcan_dir(&paths).expect("vault should initialize");
    let mut core = McpServerCore::new(
        &paths,
        Some("daily-wiki-agent"),
        &[McpToolPackArg::Index],
        McpToolPackModeArg::Static,
    )
    .expect("MCP core should initialize");
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "index_scan",
            "arguments": {}
        }
    });

    let messages = core.process_request_with_timeout(request, Duration::ZERO);

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["id"].as_i64(), Some(7));
    assert_eq!(
        messages[0]["result"]["structuredContent"]["timed_out"].as_bool(),
        Some(true)
    );
    assert_eq!(messages[0]["result"]["isError"].as_bool(), Some(true));
}

#[test]
fn catalog_pack_selection_and_permissions_filter_builtin_tools() {
    let selected =
        resolve_selected_tool_packs(&[McpToolPackArg::NotesRead], McpToolPackMode::Adaptive);
    assert!(selected.contains(&McpToolPack::NotesRead));
    assert!(selected.contains(&McpToolPack::ToolPacks));

    let readonly = PermissionProfile::readonly();
    let visible = visible_tool_catalog(&selected, &readonly)
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(visible.contains(&"note_get"));
    assert!(visible.contains(&"tool_packs"));
    assert!(!visible.contains(&"note_set"));
}

#[test]
fn sync_tools_require_full_read_and_git_and_remain_mutation_free() {
    let selected = resolve_selected_tool_packs(&[McpToolPackArg::Sync], McpToolPackMode::Static);
    let visible = visible_tool_catalog(&selected, &PermissionProfile::unrestricted())
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert_eq!(
        visible,
        vec!["sync_status", "sync_plan", "sync_doctor", "sync_conflicts"]
    );
    assert!(visible_tool_catalog(&selected, &PermissionProfile::readonly()).is_empty());

    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let paths = VaultPaths::new(tmp.path());
    let mut core = McpServerCore::new(
        &paths,
        None,
        &[McpToolPackArg::Sync],
        McpToolPackModeArg::Static,
    )
    .expect("MCP core should initialize");
    let result = core
        .call_tool("sync_doctor", &Map::new())
        .expect("read-only sync doctor should execute");
    assert_eq!(result["isError"].as_bool(), Some(false));
    assert_eq!(result["structuredContent"]["healthy"], false);
    assert!(!tmp.path().join(".git").exists());
    assert!(!tmp.path().join(".vulcan").exists());
}

#[test]
fn daily_wiki_agent_can_use_index_scan_when_index_pack_is_selected() {
    let tmp = tempfile::tempdir().expect("tempdir should be created");
    let paths = VaultPaths::new(tmp.path());
    vulcan_core::initialize_vulcan_dir(&paths).expect("vault should initialize");
    fs::write(tmp.path().join("Home.md"), "# Home\n").expect("note should write");
    let mut core = McpServerCore::new(
        &paths,
        Some("daily-wiki-agent"),
        &[McpToolPackArg::Index],
        McpToolPackModeArg::Static,
    )
    .expect("MCP core should initialize");

    let tools = core.visible_tools();
    assert!(
        tools.iter().any(|tool| tool.name == "index_scan"),
        "index pack should expose index_scan under daily-wiki-agent"
    );
    let result = core
        .call_tool("index_scan", &Map::new())
        .expect("daily-wiki-agent should be allowed to scan");
    assert_eq!(result["isError"].as_bool(), Some(false));
    assert_eq!(result["structuredContent"]["added"].as_u64(), Some(1));
}
