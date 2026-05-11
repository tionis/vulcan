use super::*;
use crate::McpToolPackModeArg;
use vulcan_core::PermissionProfile;

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
        request_timeout: DEFAULT_MCP_REQUEST_TIMEOUT,
    }
}

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

#[test]
fn oauth_options_reject_shared_token_and_plain_http_public_url() {
    let paths = VaultPaths::new(".");
    let mut with_shared_token = oauth_options();
    with_shared_token.auth_token = Some("secret".to_string());
    assert!(build_mcp_oauth_validator(&paths, &with_shared_token)
        .unwrap_err()
        .to_string()
        .contains("mutually exclusive"));

    let mut plain_http = oauth_options();
    plain_http.public_url = Some("http://wiki.example.test/mcp".to_string());
    assert!(build_mcp_oauth_validator(&paths, &plain_http)
        .unwrap_err()
        .to_string()
        .contains("HTTPS"));
}

#[test]
fn oauth_options_require_audience_and_allowed_principal() {
    let paths = VaultPaths::new(".");
    let mut missing_audience = oauth_options();
    missing_audience.oauth_audience.clear();
    assert!(build_mcp_oauth_validator(&paths, &missing_audience)
        .unwrap_err()
        .to_string()
        .contains("--oauth-audience"));

    let mut missing_principal = oauth_options();
    missing_principal.oauth_allowed_sub.clear();
    missing_principal.oauth_allowed_email.clear();
    assert!(build_mcp_oauth_validator(&paths, &missing_principal)
        .unwrap_err()
        .to_string()
        .contains("--oauth-allowed-sub"));
}

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

    assert!(build_mcp_oauth_validator(&paths, &options)
        .expect("DCR local issuer should initialize")
        .is_some());
    let secret_path = oauth_issuer_secret_path(&paths);
    let first_secret = fs::read_to_string(&secret_path).expect("issuer secret should be persisted");
    assert!(!first_secret.trim().is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&secret_path)
            .expect("issuer secret metadata should be readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    assert!(build_mcp_oauth_validator(&paths, &options)
        .expect("DCR local issuer should reuse persisted secret")
        .is_some());
    let second_secret = fs::read_to_string(&secret_path).expect("issuer secret should still exist");
    assert_eq!(first_secret, second_secret);
}

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
    assert!(visible.contains(&"tool_pack_list"));
    assert!(!visible.contains(&"note_set"));
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
