use super::*;
use crate::McpToolPackModeArg;
use std::io::Read;
use vulcan_core::{PermissionProfile, TasksQueryResult};
use vulcan_daemon::mcp_http_codec::MAX_MCP_HTTP_BODY_BYTES;

#[test]
fn stdio_and_http_reads_refuse_a_pending_ordinary_write_journal() {
    #[derive(serde::Serialize)]
    struct JournalFixture<'a> {
        version: u32,
        transaction_id: &'a str,
        changes: &'a [vulcan_core::ordinary_write::OrdinaryWriteChange],
        digest: String,
    }

    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    vulcan_core::initialize_vulcan_dir(&paths).expect("initialize vault");
    fs::write(temporary.path().join("Inbox.md"), "old task\n").expect("source");
    fs::write(temporary.path().join("Task.md"), "new task\n").expect("published target");
    let changes = vec![
        vulcan_core::ordinary_write::OrdinaryWriteChange {
            path: "Task.md".to_string(),
            before: None,
            after: Some("new task\n".to_string()),
        },
        vulcan_core::ordinary_write::OrdinaryWriteChange {
            path: "Inbox.md".to_string(),
            before: Some("old task\n".to_string()),
            after: Some("[[Task]]\n".to_string()),
        },
    ];
    let transaction_id = Ulid::new().to_string();
    let mut journal = JournalFixture {
        version: 1,
        transaction_id: &transaction_id,
        changes: &changes,
        digest: String::new(),
    };
    journal.digest = blake3::hash(&serde_json::to_vec(&journal).expect("journal bytes"))
        .to_hex()
        .to_string();
    let state = paths
        .operational_state_dir()
        .expect("operational state")
        .join("ordinary-write");
    fs::create_dir_all(&state).expect("journal state");
    let journal_path = state.join("journal.json");
    fs::write(
        &journal_path,
        serde_json::to_vec(&journal).expect("sealed journal"),
    )
    .expect("journal");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600))
            .expect("owner-only journal");
    }

    let mut core = McpServerCore::new(
        &paths,
        Some("readonly"),
        &[McpToolPackArg::NotesRead],
        McpToolPackModeArg::Static,
    )
    .expect("MCP core");
    let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let stdio = core.process_request(request.clone());
    assert_eq!(stdio[0]["error"]["code"], -32603);
    assert!(stdio[0]["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("ordinary write journal is pending"));
    let http = core
        .process_http_request(&request)
        .expect_err("HTTP read must also be blocked");
    assert_eq!(http["error"]["code"], -32603);
    let notification = serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    assert!(core.process_request(notification.clone()).is_empty());
    assert!(core
        .process_http_request(&notification)
        .expect("notification has no JSON-RPC error response")
        .response
        .is_none());
    assert!(journal_path.exists());

    vulcan_core::ordinary_write::recover_ordinary_write_batch(&paths).expect("recovery");
    assert!(!journal_path.exists());
    assert!(core.process_http_request(&request).is_ok());
}

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
                hosted: None,
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
#[allow(clippy::too_many_lines)] // Covers live listener isolation, grant attenuation, and revocation together.
fn two_named_hosted_http_listeners_bind_and_stop_independently() {
    let temporary = tempfile::tempdir().expect("temporary state");
    let runtime = tokio::runtime::Runtime::new().expect("hosted runtime");
    let scheduler =
        Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).expect("scheduler"));
    let mut listeners = Vec::new();
    for name in ["first", "second"] {
        let root = temporary.path().join(name);
        fs::create_dir_all(&root).expect("vault root");
        let paths = VaultPaths::new(&root);
        let wiki_id = vulcan_daemon::registry::WikiId::parse(name).expect("wiki ID");
        let named = NamedMcpRuntime {
            remote_id: vulcan_daemon::mcp_remote::McpRemoteId::parse(name).expect("remote ID"),
            vaults: BTreeMap::from([(
                wiki_id,
                NamedMcpVaultRuntime {
                    paths: paths.clone(),
                    ceiling_profile: "readonly".to_string(),
                    default_profile: "readonly".to_string(),
                    eligible_tool_packs: vec!["notes-read".to_string()],
                },
            )]),
            authorization_store: McpAuthorizationStore::at(temporary.path().join("state")),
        };
        let mut options = oauth_options();
        options.bind = "127.0.0.1:0".to_string();
        options.endpoint = format!("/{name}");
        options.public_url = Some(format!("https://mcp.example.test/{name}"));
        options.oauth_issuer = None;
        options.oauth_audience.clear();
        options.oauth_jwks_url = None;
        options.oauth_allowed_sub.clear();
        options.oauth_local_client_secret = Some(format!("{name}-client-secret"));
        options.oauth_local_approval_token = Some(format!("{name}-approval-token"));
        options.oauth_local_subject = Some(format!("https://identity.example.test/{name}"));
        options.instance_id = Some(Ulid::new());
        options.oauth_storage_dir = Some(temporary.path().join("state").join(name));
        fs::create_dir_all(options.oauth_storage_dir.as_ref().expect("OAuth state"))
            .expect("OAuth state directory");
        write_secret_file(
            &oauth_signing_key_path(&paths, &options),
            "shared-test-signing-key",
        )
        .expect("shared signing key");
        let token = named_listener_test_token(
            &paths,
            &named,
            &options,
            name,
            "readonly",
            &["notes-read"],
            &["mcp:tools"],
        );
        let hosted = named_listener_hosted_execution(
            &scheduler,
            runtime.handle(),
            &temporary.path().join("operations").join(name),
        );
        let stop = Arc::new(ShutdownSignal::new(false));
        let runner_stop = Arc::clone(&stop);
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (done_sender, done_receiver) = mpsc::channel();
        let runner = thread::spawn(move || {
            let on_ready = |address| ready_sender.send(address).map_err(CliError::operation);
            let result = run_mcp_http_server_with_named_runtime(
                &paths,
                Some("readonly"),
                &[McpToolPackArg::NotesRead],
                McpToolPackModeArg::Static,
                &options,
                named,
                McpHttpLifecycle {
                    stop: Some(&runner_stop),
                    ready: Some(&on_ready),
                    hosted: Some(hosted),
                },
            );
            done_sender.send(result).expect("completion receiver");
        });
        let address = ready_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|error| {
                panic!(
                    "named listener readiness failed: {error}; startup: {:?}",
                    done_receiver.recv_timeout(Duration::from_secs(1))
                )
            });
        listeners.push((address, stop, done_receiver, runner, token));
    }

    assert_ne!(listeners[0].0, listeners[1].0);
    assert_named_listener_identity(listeners[0].0, "first");
    assert_named_listener_identity(listeners[1].0, "second");
    let first_init = named_listener_initialize(listeners[0].0, "first", &listeners[0].4);
    let second_init = named_listener_initialize(listeners[1].0, "second", &listeners[1].4);
    assert!(first_init.starts_with("HTTP/1.1 200"), "{first_init}");
    assert!(second_init.starts_with("HTTP/1.1 200"), "{second_init}");
    for (name, listener, initialize) in [
        ("first", &listeners[0], &first_init),
        ("second", &listeners[1], &second_init),
    ] {
        let session_id = named_listener_session_id(initialize);
        let tools = named_listener_tools(listener.0, name, &listener.4, &session_id);
        assert!(tools.starts_with("HTTP/1.1 200"), "{tools}");
        assert!(tools.contains("\"name\":\"note_get\""), "{tools}");
        assert!(!tools.contains("\"name\":\"note_create\""), "{tools}");
    }
    assert!(
        named_listener_initialize(listeners[0].0, "first", &listeners[1].4)
            .starts_with("HTTP/1.1 401")
    );
    assert!(
        named_listener_initialize(listeners[1].0, "second", &listeners[0].4)
            .starts_with("HTTP/1.1 401")
    );
    let first_session = named_listener_session_id(&first_init);
    let first_remote = vulcan_daemon::mcp_remote::McpRemoteId::parse("first").expect("remote ID");
    let store = McpAuthorizationStore::at(temporary.path().join("state"));
    let first_grant = store
        .list_grants(Some(&first_remote))
        .expect("first grants")
        .pop()
        .expect("first grant");
    store
        .revoke_grant(first_grant.id, current_unix_timestamp(), false)
        .expect("revoke first grant");
    assert!(
        named_listener_tools(listeners[0].0, "first", &listeners[0].4, &first_session)
            .starts_with("HTTP/1.1 401")
    );
    assert!(named_listener_tools(
        listeners[1].0,
        "second",
        &listeners[1].4,
        &named_listener_session_id(&second_init),
    )
    .starts_with("HTTP/1.1 200"));
    listeners[0].1.cancel();
    listeners[0]
        .2
        .recv_timeout(Duration::from_secs(5))
        .expect("first listener stopped")
        .expect("first listener stopped cleanly");
    assert_eq!(
        named_listener_resource_metadata(listeners[1].0, "second")["resource"],
        "https://mcp.example.test/second"
    );
    listeners[1].1.cancel();
    listeners[1]
        .2
        .recv_timeout(Duration::from_secs(5))
        .expect("second listener stopped")
        .expect("second listener stopped cleanly");
    for (_, _, _, runner, _) in listeners {
        runner.join().expect("listener thread");
    }
}

#[cfg(feature = "oauth")]
#[test]
#[allow(clippy::too_many_lines)] // Launches the same persisted definition through both real ownership paths.
fn named_remote_foreground_and_resident_launches_enforce_the_same_grant() {
    let temporary = tempfile::tempdir().expect("temporary state");
    let vault = temporary.path().join("vault");
    fs::create_dir_all(&vault).expect("vault root");
    let paths = VaultPaths::new(&vault);
    vulcan_core::initialize_vulcan_dir(&paths).expect("initialize vault");
    fs::write(vault.join("LargeOwner.md"), "x".repeat(70_000)).expect("owner large-result fixture");
    fs::write(vault.join("LargePeer.md"), "y".repeat(70_000)).expect("peer large-result fixture");
    fs::create_dir_all(vault.join("AI/Prompts")).expect("prompt directory");
    fs::write(
        vault.join("AI/Prompts/summary.md"),
        "---\nname: summary\ntitle: Summarize Note\ndescription: Summarize a note\nversion: 1\nrole: user\narguments:\n  - name: note\n    required: true\n---\nSummarize {{note}}.\n",
    )
    .expect("prompt fixture");
    let process = DaemonProcessContext {
        registry: vulcan_daemon::registry::WikiRegistry::at(temporary.path().join("daemon.toml")),
        state_root: temporary.path().join("state"),
        verbose: false,
    };
    let wiki_id = vulcan_daemon::registry::WikiId::parse("personal").expect("wiki ID");
    process
        .registry
        .add(
            &vulcan_daemon::registry::AddWikiRequest {
                id: wiki_id.clone(),
                path: vault,
                profile: None,
                groups: Vec::new(),
                git_dir: None,
                permissions_profile: None,
                sync_backend: Some("none".to_string()),
                platform_profile: None,
            },
            false,
        )
        .expect("wiki registration");
    let team_root = temporary.path().join("team-vault");
    fs::create_dir_all(&team_root).expect("team vault root");
    let team_paths = VaultPaths::new(&team_root);
    vulcan_core::initialize_vulcan_dir(&team_paths).expect("initialize team vault");
    let team_id = vulcan_daemon::registry::WikiId::parse("team").expect("team wiki ID");
    process
        .registry
        .add(
            &vulcan_daemon::registry::AddWikiRequest {
                id: team_id.clone(),
                path: team_root,
                profile: None,
                groups: Vec::new(),
                git_dir: None,
                permissions_profile: None,
                sync_backend: Some("none".to_string()),
                platform_profile: None,
            },
            false,
        )
        .expect("team wiki registration");
    let reserved = TcpListener::bind("127.0.0.1:0").expect("reserve listener port");
    let address = reserved.local_addr().expect("listener address");
    drop(reserved);
    let remote = process
        .registry
        .add_mcp_remote(
            vulcan_daemon::mcp_remote::AddMcpRemoteRequest {
                id: vulcan_daemon::mcp_remote::McpRemoteId::parse("parity").expect("remote ID"),
                bind: address.to_string(),
                public_url: "https://mcp.example.test/parity".to_string(),
                authentication: McpRemoteAuthentication::IndieAuth {
                    identity: "https://identity.example.test/alice".to_string(),
                },
                vaults: vec![
                    vulcan_daemon::mcp_remote::McpRemoteVault {
                        wiki_id: wiki_id.clone(),
                        ceiling_profile: "unrestricted".to_string(),
                        default_profile: "readonly".to_string(),
                        tool_packs: vec![
                            "notes-read".to_string(),
                            "notes-write".to_string(),
                            "notes-manage".to_string(),
                            "tasks".to_string(),
                        ],
                    },
                    vulcan_daemon::mcp_remote::McpRemoteVault {
                        wiki_id: team_id.clone(),
                        ceiling_profile: "unrestricted".to_string(),
                        default_profile: "readonly".to_string(),
                        tool_packs: vec![
                            "notes-read".to_string(),
                            "notes-write".to_string(),
                            "notes-manage".to_string(),
                            "tasks".to_string(),
                        ],
                    },
                ],
            },
            false,
        )
        .expect("remote registration");
    let mut token_options = oauth_options();
    token_options.public_url = Some(remote.public_url.clone());
    token_options.instance_id = Some(remote.instance_id);
    token_options.oauth_storage_dir = Some(
        process
            .state_root
            .join("mcp-remotes")
            .join(remote.id.as_str()),
    );
    token_options.oauth_local_subject = Some("https://identity.example.test/alice".to_string());
    token_options.oauth_local_client_secret = Some("test-client-secret".to_string());
    token_options.oauth_local_approval_token = Some("test-approval-token".to_string());
    let named = NamedMcpRuntime {
        remote_id: remote.id.clone(),
        vaults: BTreeMap::from([
            (
                wiki_id,
                NamedMcpVaultRuntime {
                    paths: paths.clone(),
                    ceiling_profile: "unrestricted".to_string(),
                    default_profile: "readonly".to_string(),
                    eligible_tool_packs: vec![
                        "notes-read".to_string(),
                        "notes-write".to_string(),
                        "notes-manage".to_string(),
                        "tasks".to_string(),
                    ],
                },
            ),
            (
                team_id,
                NamedMcpVaultRuntime {
                    paths: team_paths.clone(),
                    ceiling_profile: "unrestricted".to_string(),
                    default_profile: "readonly".to_string(),
                    eligible_tool_packs: vec![
                        "notes-read".to_string(),
                        "notes-write".to_string(),
                        "notes-manage".to_string(),
                        "tasks".to_string(),
                    ],
                },
            ),
        ]),
        authorization_store: McpAuthorizationStore::at(&process.state_root),
    };
    let token = named_listener_test_token(
        &paths,
        &named,
        &token_options,
        "personal",
        "readonly",
        &["notes-read", "tasks"],
        &["mcp:tools", "mcp:resources", "mcp:prompts"],
    );
    let write_token = named_listener_test_token(
        &paths,
        &named,
        &token_options,
        "personal",
        "unrestricted",
        &["notes-write", "notes-manage", "tasks"],
        &["mcp:tools"],
    );
    let team_token = named_listener_test_token(
        &team_paths,
        &named,
        &token_options,
        "team",
        "unrestricted",
        &["notes-write"],
        &["mcp:tools"],
    );
    let endpoints = (
        "https://identity.example.test/authorize".to_string(),
        "https://identity.example.test/token".to_string(),
    );

    let stop = Arc::new(ShutdownSignal::new(false));
    let runner_stop = Arc::clone(&stop);
    let runner_process = process.clone();
    let runner_remote = remote.clone();
    let runner_endpoints = endpoints.clone();
    let (ready_sender, ready_receiver) = mpsc::channel();
    let runner = thread::spawn(move || {
        let on_ready = |bound| ready_sender.send(bound).map_err(CliError::operation);
        run_named_mcp_remote_with_endpoints(
            &runner_process,
            &runner_remote,
            Some(&runner_stop),
            Some(&on_ready),
            None,
            Some(&runner_endpoints),
        )
    });
    assert_eq!(
        ready_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("foreground readiness"),
        address
    );
    let foreground_init = named_listener_initialize(address, "parity", &token);
    assert!(
        foreground_init.starts_with("HTTP/1.1 200"),
        "{foreground_init}"
    );
    let foreground_session = named_listener_session_id(&foreground_init);
    let foreground_tools = named_listener_tools(address, "parity", &token, &foreground_session);
    assert!(
        foreground_tools.starts_with("HTTP/1.1 200"),
        "{foreground_tools}"
    );
    assert!(foreground_tools.contains("\"name\":\"task_list\""));
    assert!(!foreground_tools.contains("\"name\":\"task_create\""));
    let foreground_resources = named_listener_method(
        address,
        "parity",
        &token,
        &foreground_session,
        "resources/list",
    );
    assert!(
        foreground_resources.starts_with("HTTP/1.1 200"),
        "{foreground_resources}"
    );
    let foreground_prompts = named_listener_method(
        address,
        "parity",
        &token,
        &foreground_session,
        "prompts/list",
    );
    assert!(
        foreground_prompts.starts_with("HTTP/1.1 200"),
        "{foreground_prompts}"
    );
    assert!(foreground_prompts.contains("\"name\":\"summary\""));
    let foreground_paginated = ["tools/list", "resources/list", "prompts/list"]
        .into_iter()
        .map(|method| {
            let response = named_listener_method_with_params(
                address,
                "parity",
                &token,
                &foreground_session,
                method,
                Some(serde_json::json!({"cursor": "1"})),
            );
            assert!(response.starts_with("HTTP/1.1 200"), "{method}: {response}");
            (method, response)
        })
        .collect::<Vec<_>>();
    let foreground_prompt = named_listener_method_with_params(
        address,
        "parity",
        &token,
        &foreground_session,
        "prompts/get",
        Some(serde_json::json!({"name": "summary", "arguments": {"note": "Alpha.md"}})),
    );
    assert!(
        foreground_prompt.contains("Summarize Alpha.md."),
        "{foreground_prompt}"
    );
    let foreground_resource = named_listener_method_with_params(
        address,
        "parity",
        &token,
        &foreground_session,
        "resources/read",
        Some(serde_json::json!({"uri": "vulcan://assistant/prompts/index"})),
    );
    let resource_json: serde_json::Value = serde_json::from_str(
        foreground_resource
            .split_once("\r\n\r\n")
            .expect("resource response body")
            .1,
    )
    .expect("resource response JSON");
    assert!(resource_json["result"]["contents"][0]["text"]
        .as_str()
        .expect("resource text")
        .contains("\"summary\""));
    assert_named_listener_large_resources_are_session_scoped(address, &token);
    let denied_foreground = named_listener_create_note(
        address,
        "parity",
        &token,
        &foreground_session,
        "DeniedForeground.md",
    );
    assert!(
        denied_foreground.contains("\"isError\":true"),
        "{denied_foreground}"
    );
    assert!(!paths.vault_root().join("DeniedForeground.md").exists());
    let foreground_task_init = named_listener_initialize(address, "parity", &token);
    assert!(foreground_task_init.starts_with("HTTP/1.1 200"));
    let denied_foreground_task = named_listener_call_tool(
        address,
        "parity",
        &token,
        &named_listener_session_id(&foreground_task_init),
        "task_create",
        serde_json::json!({"text": "Denied", "note": "DeniedForegroundTasks.md"}),
    );
    assert!(
        denied_foreground_task.contains("\"isError\":true"),
        "{denied_foreground_task}"
    );
    assert!(!paths.vault_root().join("DeniedForegroundTasks.md").exists());
    let foreground_write_init = named_listener_initialize(address, "parity", &write_token);
    assert!(
        foreground_write_init.starts_with("HTTP/1.1 200"),
        "{foreground_write_init}"
    );
    let foreground_write = named_listener_create_note(
        address,
        "parity",
        &write_token,
        &named_listener_session_id(&foreground_write_init),
        "Foreground.md",
    );
    assert!(
        foreground_write.starts_with("HTTP/1.1 200"),
        "{foreground_write}"
    );
    assert!(
        foreground_write.contains("\"isError\":false"),
        "{foreground_write}"
    );
    assert!(paths.vault_root().join("Foreground.md").is_file());
    let foreground_write_session = named_listener_session_id(&foreground_write_init);
    let foreground_write_tools =
        named_listener_tools(address, "parity", &write_token, &foreground_write_session);
    assert!(foreground_write_tools.contains("\"name\":\"note_create\""));
    assert!(foreground_write_tools.contains("\"name\":\"task_create\""));
    assert!(!foreground_write_tools.contains("\"name\":\"note_get\""));
    assert!(named_listener_method(
        address,
        "parity",
        &write_token,
        &foreground_write_session,
        "resources/list"
    )
    .starts_with("HTTP/1.1 403"));
    named_listener_append_and_patch_note(
        address,
        &write_token,
        &foreground_write_session,
        &paths,
        "Foreground.md",
    );
    let foreground_set = named_listener_call_tool(
        address,
        "parity",
        &write_token,
        &foreground_write_session,
        "note_set",
        serde_json::json!({
            "note": "Foreground.md", "content": "Foreground replacement.\n",
            "confirm": true, "no_commit": true
        }),
    );
    assert!(
        foreground_set.contains("\"isError\":false"),
        "{foreground_set}"
    );
    assert_eq!(
        fs::read_to_string(paths.vault_root().join("Foreground.md"))
            .expect("replaced note")
            .replace("\r\n", "\n"),
        "Foreground replacement.\n"
    );
    let foreground_delete = named_listener_call_tool(
        address,
        "parity",
        &write_token,
        &foreground_write_session,
        "note_delete",
        serde_json::json!({"note": "Foreground.md", "confirm": true, "no_commit": true}),
    );
    assert!(
        foreground_delete.contains("\"isError\":false"),
        "{foreground_delete}"
    );
    assert!(!paths.vault_root().join("Foreground.md").exists());
    named_listener_task_lifecycle(
        address,
        &write_token,
        &foreground_write_session,
        &paths,
        "Foreground",
    );
    let foreground_team_init = named_listener_initialize(address, "parity", &team_token);
    assert!(
        foreground_team_init.starts_with("HTTP/1.1 200"),
        "{foreground_team_init}"
    );
    let foreground_team_session = named_listener_session_id(&foreground_team_init);
    assert!(
        named_listener_tools(address, "parity", &team_token, &foreground_session)
            .starts_with("HTTP/1.1 404")
    );
    let foreground_team_write = named_listener_create_note(
        address,
        "parity",
        &team_token,
        &foreground_team_session,
        "TeamForeground.md",
    );
    assert!(
        foreground_team_write.contains("\"isError\":false"),
        "{foreground_team_write}"
    );
    assert!(team_paths.vault_root().join("TeamForeground.md").is_file());
    assert!(!paths.vault_root().join("TeamForeground.md").exists());
    assert_named_listener_prompt_change_notifications(address, &token, &paths, "foreground");
    stop.cancel();
    runner
        .join()
        .expect("foreground thread")
        .expect("foreground stop");

    let write_grant = named
        .authorization_store
        .list_grants(Some(&remote.id))
        .expect("persisted grants")
        .into_iter()
        .find(|grant| grant.permission_profile == "unrestricted")
        .expect("write grant");
    let grant_permissions = resolve_permission_profile(&paths, Some("unrestricted"))
        .expect("write permissions")
        .grant;
    let interrupted_context = ExecutionContext::new(
        ExecutionVaultIdentity::resolve(paths.vault_root(), None, None).expect("vault identity"),
        ExecutionAuthority::Caller {
            principal_id: "https://identity.example.test/alice".to_string(),
            credential_id: Some(write_grant.id.to_string()),
            permission_ceiling: grant_permissions.clone(),
        },
        grant_permissions,
        ExecutionIdentity::new(format!("mcp:{}", remote.instance_id)),
        Some(remote.public_url.clone()),
        ExecutionRetryClass::IndeterminateAfterDispatch,
        ExecutionCancellationToken::default(),
        None,
    )
    .expect("interrupted operation context");
    let operation_id = interrupted_context.identity.operation_id.clone();
    let ledger = HostedJobLedger::at(
        process
            .state_root
            .join("mcp-remotes")
            .join(remote.id.as_str())
            .join("operations"),
    );
    ledger
        .register(&interrupted_context, current_unix_millis())
        .expect("register pending operation");
    ledger
        .mark_running(&operation_id, current_unix_millis())
        .expect("mark pending operation running");

    let runtime = tokio::runtime::Runtime::new().expect("resident runtime");
    let scheduler =
        Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).expect("scheduler"));
    let resident = resident_named_mcp_service_with_endpoints(
        &process,
        &[remote],
        scheduler,
        runtime.handle().clone(),
        Some(endpoints),
    )
    .expect("resident registration")
    .expect("resident service");
    let dependency = ServiceRegistration::new(
        ServiceDefinition {
            id: ServiceId::parse("worker.sync-trigger").expect("service ID"),
            service_kind: "worker".to_string(),
            scope: ServiceScope::Global,
            enabled: true,
            required: true,
            dependencies: Vec::new(),
            restart: RestartPolicy::Never,
        },
        |context| {
            context.ready()?;
            while !context.stop().wait_timeout(Duration::from_millis(25)) {}
            Ok(())
        },
    );
    let supervisor = vulcan_daemon::host::HostSupervisor::start(
        vec![dependency, resident],
        Duration::from_secs(10),
    )
    .expect("resident host startup");
    assert_eq!(
        ledger
            .load(&operation_id)
            .expect("recovered operation")
            .state,
        vulcan_daemon::hosted_jobs::HostedJobState::Interrupted
    );
    let visible_status =
        named_listener_operation_status(address, "parity", &write_token, &operation_id);
    assert!(
        visible_status.starts_with("HTTP/1.1 200"),
        "{visible_status}"
    );
    assert!(
        visible_status.contains("\"state\":\"interrupted\""),
        "{visible_status}"
    );
    assert!(
        named_listener_operation_status(address, "parity", &token, &operation_id)
            .starts_with("HTTP/1.1 404")
    );
    let resident_init = named_listener_initialize(address, "parity", &token);
    assert!(resident_init.starts_with("HTTP/1.1 200"), "{resident_init}");
    let resident_session = named_listener_session_id(&resident_init);
    let resident_tools = named_listener_tools(address, "parity", &token, &resident_session);
    assert!(
        resident_tools.starts_with("HTTP/1.1 200"),
        "{resident_tools}"
    );
    assert!(resident_tools.contains("\"name\":\"note_get\""));
    assert!(resident_tools.contains("\"name\":\"task_list\""));
    assert!(!resident_tools.contains("\"name\":\"note_create\""));
    assert!(!resident_tools.contains("\"name\":\"task_create\""));
    let denied_resident = named_listener_create_note(
        address,
        "parity",
        &token,
        &resident_session,
        "DeniedResident.md",
    );
    assert!(
        denied_resident.contains("\"isError\":true"),
        "{denied_resident}"
    );
    assert!(!paths.vault_root().join("DeniedResident.md").exists());
    let resident_task_init = named_listener_initialize(address, "parity", &token);
    assert!(resident_task_init.starts_with("HTTP/1.1 200"));
    let denied_resident_task = named_listener_call_tool(
        address,
        "parity",
        &token,
        &named_listener_session_id(&resident_task_init),
        "task_create",
        serde_json::json!({"text": "Denied", "note": "DeniedResidentTasks.md"}),
    );
    assert!(
        denied_resident_task.contains("\"isError\":true"),
        "{denied_resident_task}"
    );
    assert!(!paths.vault_root().join("DeniedResidentTasks.md").exists());
    assert_eq!(
        foreground_tools
            .split_once("\r\n\r\n")
            .expect("foreground body")
            .1,
        resident_tools
            .split_once("\r\n\r\n")
            .expect("resident body")
            .1,
    );
    let resident_resource_session =
        named_listener_session_id(&named_listener_initialize(address, "parity", &token));
    for (method, foreground) in [
        ("resources/list", &foreground_resources),
        ("prompts/list", &foreground_prompts),
    ] {
        let resident_response = named_listener_method(
            address,
            "parity",
            &token,
            &resident_resource_session,
            method,
        );
        assert!(
            resident_response.starts_with("HTTP/1.1 200"),
            "{resident_response}"
        );
        assert_eq!(
            foreground
                .split_once("\r\n\r\n")
                .expect("foreground body")
                .1,
            resident_response
                .split_once("\r\n\r\n")
                .expect("resident body")
                .1,
            "{method} changed between foreground and resident hosting"
        );
    }
    for (method, foreground) in &foreground_paginated {
        let resident_response = named_listener_method_with_params(
            address,
            "parity",
            &token,
            &resident_resource_session,
            method,
            Some(serde_json::json!({"cursor": "1"})),
        );
        assert!(
            resident_response.starts_with("HTTP/1.1 200"),
            "{resident_response}"
        );
        assert_eq!(
            foreground
                .split_once("\r\n\r\n")
                .expect("foreground body")
                .1,
            resident_response
                .split_once("\r\n\r\n")
                .expect("resident body")
                .1,
            "{method} pagination changed between foreground and resident hosting"
        );
    }
    for (method, params, foreground) in [
        (
            "prompts/get",
            serde_json::json!({"name": "summary", "arguments": {"note": "Alpha.md"}}),
            &foreground_prompt,
        ),
        (
            "resources/read",
            serde_json::json!({"uri": "vulcan://assistant/prompts/index"}),
            &foreground_resource,
        ),
    ] {
        let resident_response = named_listener_method_with_params(
            address,
            "parity",
            &token,
            &resident_resource_session,
            method,
            Some(params),
        );
        assert!(
            resident_response.starts_with("HTTP/1.1 200"),
            "{resident_response}"
        );
        assert_eq!(
            foreground
                .split_once("\r\n\r\n")
                .expect("foreground body")
                .1,
            resident_response
                .split_once("\r\n\r\n")
                .expect("resident body")
                .1,
            "{method} changed between foreground and resident hosting"
        );
    }
    assert_named_listener_large_resources_are_session_scoped(address, &token);
    assert!(named_listener_method(
        address,
        "parity",
        &write_token,
        &resident_resource_session,
        "resources/list"
    )
    .starts_with("HTTP/1.1 403"));
    assert!(
        named_listener_tools(address, "parity", &token, &foreground_session)
            .starts_with("HTTP/1.1 404")
    );
    let resident_write_init = named_listener_initialize(address, "parity", &write_token);
    assert!(
        resident_write_init.starts_with("HTTP/1.1 200"),
        "{resident_write_init}"
    );
    let resident_write_session = named_listener_session_id(&resident_write_init);
    let resident_write_tools =
        named_listener_tools(address, "parity", &write_token, &resident_write_session);
    assert_eq!(
        foreground_write_tools
            .split_once("\r\n\r\n")
            .expect("foreground write tools")
            .1,
        resident_write_tools
            .split_once("\r\n\r\n")
            .expect("resident write tools")
            .1,
    );
    let resident_write = named_listener_create_note(
        address,
        "parity",
        &write_token,
        &resident_write_session,
        "Resident.md",
    );
    assert!(
        resident_write.starts_with("HTTP/1.1 200"),
        "{resident_write}"
    );
    assert!(
        resident_write.contains("\"isError\":false"),
        "{resident_write}"
    );
    assert!(paths.vault_root().join("Resident.md").is_file());
    named_listener_append_and_patch_note(
        address,
        &write_token,
        &resident_write_session,
        &paths,
        "Resident.md",
    );
    let resident_set = named_listener_call_tool(
        address,
        "parity",
        &write_token,
        &resident_write_session,
        "note_set",
        serde_json::json!({
            "note": "Resident.md", "content": "Resident replacement.\n",
            "confirm": true, "no_commit": true
        }),
    );
    assert!(resident_set.contains("\"isError\":false"), "{resident_set}");
    assert_eq!(
        fs::read_to_string(paths.vault_root().join("Resident.md"))
            .expect("replaced note")
            .replace("\r\n", "\n"),
        "Resident replacement.\n"
    );
    let resident_delete = named_listener_call_tool(
        address,
        "parity",
        &write_token,
        &resident_write_session,
        "note_delete",
        serde_json::json!({"note": "Resident.md", "confirm": true, "no_commit": true}),
    );
    assert!(
        resident_delete.contains("\"isError\":false"),
        "{resident_delete}"
    );
    assert!(!paths.vault_root().join("Resident.md").exists());
    named_listener_task_lifecycle(
        address,
        &write_token,
        &resident_write_session,
        &paths,
        "Resident",
    );
    let resident_team_init = named_listener_initialize(address, "parity", &team_token);
    assert!(
        resident_team_init.starts_with("HTTP/1.1 200"),
        "{resident_team_init}"
    );
    let resident_team_session = named_listener_session_id(&resident_team_init);
    assert!(
        named_listener_tools(address, "parity", &team_token, &resident_session)
            .starts_with("HTTP/1.1 404")
    );
    let resident_team_write = named_listener_create_note(
        address,
        "parity",
        &team_token,
        &resident_team_session,
        "TeamResident.md",
    );
    assert!(
        resident_team_write.contains("\"isError\":false"),
        "{resident_team_write}"
    );
    assert!(team_paths.vault_root().join("TeamResident.md").is_file());
    assert!(!paths.vault_root().join("TeamResident.md").exists());
    assert_named_listener_prompt_change_notifications(address, &token, &paths, "resident");
    supervisor.shutdown().expect("resident shutdown");
}

#[cfg(feature = "oauth")]
fn named_listener_test_token(
    paths: &VaultPaths,
    named: &NamedMcpRuntime,
    options: &McpHttpOptions,
    name: &str,
    profile: &str,
    packs: &[&str],
    scopes: &[&str],
) -> String {
    let store = &named.authorization_store;
    let now = current_unix_timestamp();
    let subject = options.oauth_local_subject.as_deref().expect("subject");
    let grant = store
        .create_grant(
            vulcan_daemon::mcp_state::CreateConnectionGrant {
                remote_id: named.remote_id.clone(),
                remote_instance_id: options.instance_id.expect("instance ID"),
                client_id: "vulcan-mcp".to_string(),
                subject: subject.to_string(),
                wiki_id: vulcan_daemon::registry::WikiId::parse(name).expect("wiki ID"),
                permission_profile: profile.to_string(),
                approved_permissions: resolve_permission_profile(paths, Some(profile))
                    .expect("permission profile")
                    .grant,
                tool_packs: packs.iter().map(|pack| (*pack).to_string()).collect(),
                scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
                audience: options.public_url.clone().expect("public URL"),
                created_at: now,
                expires_at: now + 86400,
            },
            false,
        )
        .expect("connection grant");
    LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
        public_url: options.public_url.clone().expect("public URL"),
        client_id: "vulcan-mcp".to_string(),
        client_secret: options
            .oauth_local_client_secret
            .clone()
            .expect("client secret"),
        signing_key: load_or_create_local_oauth_signing_key(paths, options).expect("signing key"),
        approval_token: options
            .oauth_local_approval_token
            .clone()
            .expect("approval token"),
        subject: subject.to_string(),
        email: None,
        users: Vec::new(),
        dcr_enabled: false,
    })
    .expect("issuer")
    .issue_access_token_for_authorization(
        subject,
        "vulcan-mcp",
        &scopes
            .iter()
            .map(|scope| (*scope).to_string())
            .collect::<Vec<_>>(),
        Some(grant.id.to_string()),
    )
    .expect("access token")
}

#[cfg(feature = "oauth")]
fn named_listener_initialize(address: SocketAddr, name: &str, token: &str) -> String {
    let mut stream = TcpStream::connect(address).expect("named listener active");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("response timeout");
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}
    })
    .to_string();
    write!(
        stream,
        "POST /{name} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )
    .expect("initialize request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("initialize response");
    response
}

#[cfg(feature = "oauth")]
fn named_listener_tools(address: SocketAddr, name: &str, token: &str, session_id: &str) -> String {
    named_listener_method(address, name, token, session_id, "tools/list")
}

#[cfg(feature = "oauth")]
fn named_listener_method(
    address: SocketAddr,
    name: &str,
    token: &str,
    session_id: &str,
    method: &str,
) -> String {
    named_listener_method_with_params(address, name, token, session_id, method, None)
}

#[cfg(feature = "oauth")]
fn named_listener_method_with_params(
    address: SocketAddr,
    name: &str,
    token: &str,
    session_id: &str,
    method: &str,
    params: Option<serde_json::Value>,
) -> String {
    let mut stream = TcpStream::connect(address).expect("named listener active");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("response timeout");
    let mut body = serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": method});
    if let Some(params) = params {
        body["params"] = params;
    }
    let body = body.to_string();
    write!(
        stream,
        "POST /{name} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nMcp-Session-Id: {session_id}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )
    .expect("MCP method request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("MCP method response");
    response
}

#[cfg(feature = "oauth")]
fn assert_named_listener_large_resources_are_session_scoped(address: SocketAddr, token: &str) {
    let owner = named_listener_session_id(&named_listener_initialize(address, "parity", token));
    let peer = named_listener_session_id(&named_listener_initialize(address, "parity", token));
    assert_ne!(owner, peer);

    let tool_result = |session: &str, note: &str| {
        let response = named_listener_call_tool(
            address,
            "parity",
            token,
            session,
            "note_get",
            serde_json::json!({"note": note}),
        );
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let body: serde_json::Value =
            serde_json::from_str(response.split_once("\r\n\r\n").expect("tool body").1)
                .expect("tool JSON");
        assert!(body["result"].get("structuredContent").is_none());
        body["result"]["content"]
            .as_array()
            .expect("tool content")
            .iter()
            .find(|item| item["type"] == "resource_link")
            .and_then(|item| item["uri"].as_str())
            .expect("large-result resource URI")
            .to_string()
    };
    let read = |session: &str, uri: &str| {
        let response = named_listener_method_with_params(
            address,
            "parity",
            token,
            session,
            "resources/read",
            Some(serde_json::json!({"uri": uri})),
        );
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        serde_json::from_str::<serde_json::Value>(
            response.split_once("\r\n\r\n").expect("resource body").1,
        )
        .expect("resource JSON")
    };

    let owner_uri = tool_result(&owner, "LargeOwner.md");
    assert_eq!(owner_uri, "vulcan://tool-results/1.json");
    assert_eq!(
        read(&peer, &owner_uri)["error"]["code"],
        MCP_RESOURCE_NOT_FOUND
    );
    let peer_uri = tool_result(&peer, "LargePeer.md");
    assert_eq!(peer_uri, owner_uri);
    assert!(read(&owner, &owner_uri)["result"]["contents"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains(&"x".repeat(70_000))));
    assert!(read(&peer, &peer_uri)["result"]["contents"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains(&"y".repeat(70_000))));

    let mut stream = TcpStream::connect(address).expect("named listener active");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("response timeout");
    write!(
        stream,
        "DELETE /parity HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nMcp-Session-Id: {owner}\r\nConnection: close\r\n\r\n"
    )
    .expect("delete session request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("delete response");
    assert!(response.starts_with("HTTP/1.1 204"), "{response}");
    assert!(named_listener_tools(address, "parity", token, &owner).starts_with("HTTP/1.1 404"));
    assert!(read(&peer, &peer_uri)["result"]["contents"][0]["text"]
        .as_str()
        .is_some_and(|text| text.contains(&"y".repeat(70_000))));
}

#[cfg(feature = "oauth")]
fn assert_named_listener_prompt_change_notifications(
    address: SocketAddr,
    token: &str,
    paths: &VaultPaths,
    label: &str,
) {
    let session = named_listener_session_id(&named_listener_initialize(address, "parity", token));
    let mut stream = TcpStream::connect(address).expect("named listener active");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("SSE read timeout");
    write!(
        stream,
        "GET /parity HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nMcp-Session-Id: {session}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n"
    )
    .expect("SSE request");
    let mut client = io::BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        assert!(client.read_line(&mut line).expect("SSE headers") > 0);
        headers.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    assert!(headers.starts_with("HTTP/1.1 200 OK"), "{headers}");

    let prompt_path = paths
        .vault_root()
        .join(format!("AI/Prompts/notify-{label}.md"));
    fs::write(
        &prompt_path,
        format!("---\nname: notify-{label}\nrole: user\n---\nNotify {label}.\n"),
    )
    .expect("new prompt");
    let mut prompt_changed = false;
    let mut resource_changed = false;
    for _ in 0..24 {
        let mut line = String::new();
        assert!(client.read_line(&mut line).expect("SSE event") > 0);
        if let Some(payload) = line.strip_prefix("data: ") {
            let event: serde_json::Value = serde_json::from_str(payload).expect("SSE JSON");
            prompt_changed |= event["method"] == "notifications/prompts/list_changed";
            resource_changed |= event["method"] == "notifications/resources/list_changed";
            if prompt_changed && resource_changed {
                break;
            }
        }
    }
    assert!(prompt_changed, "prompt change notification missing");
    assert!(resource_changed, "resource change notification missing");
    fs::remove_file(prompt_path).expect("remove temporary prompt");
}

#[cfg(feature = "oauth")]
fn named_listener_create_note(
    address: SocketAddr,
    name: &str,
    token: &str,
    session_id: &str,
    path: &str,
) -> String {
    named_listener_call_tool(
        address,
        name,
        token,
        session_id,
        "note_create",
        serde_json::json!({"path": path, "body": "Named MCP write.\n", "no_commit": true}),
    )
}

#[cfg(feature = "oauth")]
fn named_listener_task_lifecycle(
    address: SocketAddr,
    token: &str,
    session_id: &str,
    paths: &VaultPaths,
    prefix: &str,
) {
    let note = format!("{prefix}Tasks.md");
    let task = format!("{note}:1");
    let created = named_listener_call_tool(
        address,
        "parity",
        token,
        session_id,
        "task_create",
        serde_json::json!({"text": "Prepare parity", "note": note, "no_commit": true}),
    );
    assert!(created.contains("\"isError\":false"), "{created}");
    assert!(fs::read_to_string(paths.vault_root().join(&note))
        .expect("created task note")
        .contains("- [ ] Prepare parity"));

    let rescheduled = named_listener_call_tool(
        address,
        "parity",
        token,
        session_id,
        "task_reschedule",
        serde_json::json!({"task": task, "due": "2026-05-12", "no_commit": true}),
    );
    assert!(rescheduled.contains("\"isError\":false"), "{rescheduled}");
    assert!(fs::read_to_string(paths.vault_root().join(&note))
        .expect("rescheduled task note")
        .contains("2026-05-12"));

    let completed = named_listener_call_tool(
        address,
        "parity",
        token,
        session_id,
        "task_complete",
        serde_json::json!({"task": task, "date": "2026-05-13", "no_commit": true}),
    );
    assert!(completed.contains("\"isError\":false"), "{completed}");
    assert!(fs::read_to_string(paths.vault_root().join(note))
        .expect("completed task note")
        .contains("- [x]"));
}

#[cfg(feature = "oauth")]
fn named_listener_append_and_patch_note(
    address: SocketAddr,
    token: &str,
    session_id: &str,
    paths: &VaultPaths,
    note: &str,
) {
    let appended = named_listener_call_tool(
        address,
        "parity",
        token,
        session_id,
        "note_append",
        serde_json::json!({"note": note, "text": "Additional detail.\n", "no_commit": true}),
    );
    assert!(appended.contains("\"isError\":false"), "{appended}");
    assert!(fs::read_to_string(paths.vault_root().join(note))
        .expect("appended note")
        .contains("Additional detail."));

    let patched = named_listener_call_tool(
        address,
        "parity",
        token,
        session_id,
        "note_patch",
        serde_json::json!({
            "note": note, "find": "Additional detail.",
            "replace": "Reviewed detail.", "no_commit": true
        }),
    );
    assert!(patched.contains("\"isError\":false"), "{patched}");
    let content = fs::read_to_string(paths.vault_root().join(note)).expect("patched note");
    assert!(content.contains("Reviewed detail."));
    assert!(!content.contains("Additional detail."));
}

#[cfg(feature = "oauth")]
fn named_listener_call_tool(
    address: SocketAddr,
    name: &str,
    token: &str,
    session_id: &str,
    tool: &str,
    arguments: serde_json::Value,
) -> String {
    let mut stream = TcpStream::connect(address).expect("named listener active");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("response timeout");
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": arguments
        }
    })
    .to_string();
    write!(
        stream,
        "POST /{name} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nMcp-Session-Id: {session_id}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )
    .expect("tool-call request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("tool-call response");
    response
}

#[cfg(feature = "oauth")]
fn named_listener_operation_status(
    address: SocketAddr,
    name: &str,
    token: &str,
    operation_id: &str,
) -> String {
    let mut stream = TcpStream::connect(address).expect("named listener active");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("response timeout");
    write!(
        stream,
        "GET /{name}/operations/{operation_id} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    )
    .expect("operation-status request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("operation-status response");
    response
}

#[cfg(feature = "oauth")]
fn named_listener_session_id(response: &str) -> String {
    response
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with("mcp-session-id: ")
                .then(|| line[16..].trim().to_string())
        })
        .expect("MCP session ID")
}

#[cfg(feature = "oauth")]
fn named_listener_hosted_execution(
    scheduler: &Arc<MutationScheduler>,
    runtime: &tokio::runtime::Handle,
    ledger_path: &Path,
) -> HostedMcpExecution {
    HostedMcpExecution {
        executor: Arc::new(HostedExecutor::new(
            Arc::clone(scheduler),
            Arc::new(HostedJobLedger::at(ledger_path)),
        )),
        scheduler: Arc::clone(scheduler),
        runtime: runtime.clone(),
    }
}

#[cfg(feature = "oauth")]
fn named_listener_resource_metadata(address: SocketAddr, name: &str) -> Value {
    let mut stream = TcpStream::connect(address).expect("named listener active");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("response timeout");
    write!(
        stream,
        "GET /.well-known/oauth-protected-resource/{name} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .expect("metadata request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("metadata response");
    let response = String::from_utf8(response).expect("UTF-8 metadata response");
    let (headers, body) = response.split_once("\r\n\r\n").expect("HTTP response");
    assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
    serde_json::from_str(body).expect("resource metadata JSON")
}

#[cfg(feature = "oauth")]
fn assert_named_listener_identity(address: SocketAddr, name: &str) {
    assert_eq!(
        named_listener_resource_metadata(address, name)["resource"],
        format!("https://mcp.example.test/{name}")
    );
    assert_named_listener_auth_challenge(address, name);
}

#[cfg(feature = "oauth")]
fn assert_named_listener_auth_challenge(address: SocketAddr, name: &str) {
    let mut stream = TcpStream::connect(address).expect("named listener active");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("response timeout");
    write!(
        stream,
        "POST /{name} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    )
    .expect("unauthenticated MCP request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("auth response");
    let (headers, _) = response.split_once("\r\n\r\n").expect("HTTP response");
    assert!(headers.starts_with("HTTP/1.1 401"), "{headers}");
    assert!(headers.contains(&format!(
        "resource_metadata=\"https://mcp.example.test/.well-known/oauth-protected-resource/{name}\""
    )));
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
        hosted: None,
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
fn shutting_down_http_sessions_closes_live_sse_streams() {
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
    let context = consent_test_context(&paths, issuer);
    let authority = McpSessionAuthority::direct(
        context.instance_id,
        "credential",
        None,
        None,
        Some("readonly".to_string()),
        vec!["notes-read".to_string()],
        Vec::new(),
    );
    let session_id = Ulid::new().to_string();
    let core = McpServerCore::new(
        &paths,
        Some("readonly"),
        &[McpToolPackArg::NotesRead],
        McpToolPackModeArg::Static,
    )
    .expect("MCP core");
    let session = Arc::new(McpHttpSession::new(core, authority.clone()));
    context
        .sessions
        .lock()
        .expect("sessions lock")
        .insert(session_id.clone(), Arc::clone(&session));

    let listener = TcpListener::bind("127.0.0.1:0").expect("SSE listener");
    let address = listener.local_addr().expect("listener address");
    let server_context = context.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("SSE connection");
        let request = McpHttpRequest {
            method: "GET".to_string(),
            path: "/mcp".to_string(),
            query: String::new(),
            headers: BTreeMap::from([
                ("accept".to_string(), "text/event-stream".to_string()),
                ("mcp-session-id".to_string(), session_id),
            ]),
            body: Vec::new(),
        };
        handle_mcp_http_sse(&server_context, &request, &authority, &mut stream)
            .expect("SSE should close cleanly");
    });
    let stream = TcpStream::connect(address).expect("SSE client");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let mut client = io::BufReader::new(stream);
    let mut header = String::new();
    loop {
        let bytes = client.read_line(&mut header).expect("SSE headers");
        assert!(bytes > 0, "SSE headers ended unexpectedly");
        if header.ends_with("\r\n\r\n") {
            break;
        }
    }
    assert!(header.starts_with("HTTP/1.1 200 OK"));

    close_mcp_http_sessions(&context);
    let mut remaining = Vec::new();
    client
        .read_to_end(&mut remaining)
        .expect("SSE stream should end");
    server.join().expect("SSE handler");
    assert!(session.is_closed());
    assert!(context.sessions.lock().expect("sessions lock").is_empty());
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
        vaults: BTreeMap::from([(
            vulcan_daemon::registry::WikiId::parse("personal").expect("wiki"),
            NamedMcpVaultRuntime {
                paths: paths.clone(),
                ceiling_profile: "readonly".to_string(),
                default_profile: "readonly".to_string(),
                eligible_tool_packs: vec!["notes-read".to_string(), "search".to_string()],
            },
        )]),
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

    let mut other_named = context.named_runtime.clone().expect("named runtime");
    other_named.remote_id =
        vulcan_daemon::mcp_remote::McpRemoteId::parse("other-chatgpt").expect("other remote");
    let mut other_instance = consent_test_context(&paths, Arc::clone(&issuer));
    other_instance.named_runtime = Some(other_named.clone());
    assert_eq!(
        authenticate_mcp_http_request(&other_instance, &request)
            .expect_err("same issuer token must not cross remote instance")
            .status,
        401
    );
    let other_issuer = Arc::new(
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://mcp.example.test/other".to_string(),
            client_id: "static-client".to_string(),
            client_secret: "client-secret".to_string(),
            signing_key: "distinct-signing-key".to_string(),
            approval_token: String::new(),
            subject: "https://identity.example.test/alice".to_string(),
            email: None,
            users: Vec::new(),
            dcr_enabled: true,
        })
        .expect("other issuer"),
    );
    let mut other_audience = consent_test_context(&paths, other_issuer);
    other_audience.named_runtime = Some(other_named);
    assert_eq!(
        authenticate_mcp_http_request(&other_audience, &request)
            .expect_err("same-key token must not cross audience")
            .status,
        401
    );

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
#[allow(clippy::too_many_lines)] // Covers consent, grant persistence, session routing, and removal.
fn named_consent_routes_each_grant_to_its_selected_vault() {
    let temporary = tempfile::tempdir().expect("temporary vaults");
    let first_path = temporary.path().join("personal");
    let second_path = temporary.path().join("team");
    std::fs::create_dir_all(&first_path).expect("first vault");
    std::fs::create_dir_all(&second_path).expect("second vault");
    let first = VaultPaths::new(&first_path);
    let second = VaultPaths::new(&second_path);
    let issuer = Arc::new(
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://mcp.example.test/shared".to_string(),
            client_id: "static-client".to_string(),
            client_secret: "client-secret".to_string(),
            signing_key: "shared-remote-key".to_string(),
            approval_token: String::new(),
            subject: "https://identity.example.test/alice".to_string(),
            email: None,
            users: Vec::new(),
            dcr_enabled: true,
        })
        .expect("issuer"),
    );
    let mut context = consent_test_context(&first, Arc::clone(&issuer));
    let store = McpAuthorizationStore::at(temporary.path().join("state"));
    let personal_id = vulcan_daemon::registry::WikiId::parse("personal").expect("wiki");
    let team_id = vulcan_daemon::registry::WikiId::parse("team").expect("wiki");
    context.named_runtime = Some(NamedMcpRuntime {
        remote_id: vulcan_daemon::mcp_remote::McpRemoteId::parse("shared").expect("remote"),
        vaults: BTreeMap::from([
            (
                personal_id.clone(),
                NamedMcpVaultRuntime {
                    paths: first.clone(),
                    ceiling_profile: "readonly".to_string(),
                    default_profile: "readonly".to_string(),
                    eligible_tool_packs: vec!["notes-read".to_string()],
                },
            ),
            (
                team_id.clone(),
                NamedMcpVaultRuntime {
                    paths: second.clone(),
                    ceiling_profile: "readonly".to_string(),
                    default_profile: "readonly".to_string(),
                    eligible_tool_packs: vec!["search".to_string()],
                },
            ),
        ]),
        authorization_store: store.clone(),
    });
    let pending = LocalOAuthPendingConsent {
        client_id: "static-client".to_string(),
        redirect_uri: "https://client.example.test/callback".to_string(),
        code_challenge: "challenge".to_string(),
        subject: "https://identity.example.test/alice".to_string(),
        scopes: vec!["mcp:tools".to_string()],
        resource: "https://mcp.example.test/shared".to_string(),
        state: None,
        csrf_token: "csrf".to_string(),
        expires_at: std::time::Instant::now() + Duration::from_secs(60),
    };
    let form = local_oauth_consent_form(&context, &issuer, "transaction", &pending);
    let html = String::from_utf8(form.body).expect("consent HTML");
    assert!(html.contains("name=\"wiki_id\" value=\"personal\""));
    assert!(html.contains("name=\"wiki_id\" value=\"team\""));
    assert!(create_named_connection_grant(&context, &pending, &BTreeMap::new()).is_err());
    assert!(create_named_connection_grant(
        &context,
        &pending,
        &BTreeMap::from([("wiki_id".to_string(), "other".to_string())]),
    )
    .is_err());
    let selected = BTreeMap::from([
        ("wiki_id".to_string(), "team".to_string()),
        (
            "permission_profile_team".to_string(),
            "readonly".to_string(),
        ),
        ("pack_team_search".to_string(), "on".to_string()),
        ("expiry_days".to_string(), "7".to_string()),
    ]);
    let grant_id = create_named_connection_grant(&context, &pending, &selected)
        .expect("team consent")
        .expect("grant")
        .parse::<Ulid>()
        .expect("ULID");
    let grant = store.show_grant(grant_id).expect("durable grant");
    assert_eq!(grant.wiki_id, team_id);
    assert_eq!(grant.tool_packs, ["search"]);
    let token = issuer
        .issue_access_token_for_authorization(
            &pending.subject,
            &pending.client_id,
            &pending.scopes,
            Some(grant_id.to_string()),
        )
        .expect("access token");
    let request = McpHttpRequest {
        method: "POST".to_string(),
        path: "/mcp".to_string(),
        query: String::new(),
        headers: BTreeMap::from([("authorization".to_string(), format!("Bearer {token}"))]),
        body: Vec::new(),
    };
    let authority = authenticate_mcp_http_request(&context, &request).expect("team authority");
    let (team_session_id, session, _) = resolve_mcp_http_session(
        &context,
        &request,
        &serde_json::json!({"jsonrpc":"2.0","method":"initialize","id":1}),
        &authority,
    )
    .expect("team session");
    assert_eq!(
        session.core.lock().expect("core").paths.vault_root(),
        second.vault_root()
    );
    let personal_grant_id = create_named_connection_grant(
        &context,
        &pending,
        &BTreeMap::from([
            ("wiki_id".to_string(), "personal".to_string()),
            ("pack_personal_notes-read".to_string(), "on".to_string()),
            ("expiry_days".to_string(), "1".to_string()),
        ]),
    )
    .expect("personal consent")
    .expect("personal grant");
    let personal_token = issuer
        .issue_access_token_for_authorization(
            &pending.subject,
            &pending.client_id,
            &pending.scopes,
            Some(personal_grant_id),
        )
        .expect("personal token");
    let mut personal_request = request.clone();
    personal_request.headers.insert(
        "authorization".to_string(),
        format!("Bearer {personal_token}"),
    );
    let personal_authority =
        authenticate_mcp_http_request(&context, &personal_request).expect("personal authority");
    let (_, personal_session, _) = resolve_mcp_http_session(
        &context,
        &personal_request,
        &serde_json::json!({"jsonrpc":"2.0","method":"initialize","id":2}),
        &personal_authority,
    )
    .expect("personal session");
    assert_eq!(
        personal_session
            .core
            .lock()
            .expect("core")
            .paths
            .vault_root(),
        first.vault_root()
    );
    personal_request
        .headers
        .insert("mcp-session-id".to_string(), team_session_id);
    assert!(
        resolve_mcp_http_session(
            &context,
            &personal_request,
            &serde_json::json!({"jsonrpc":"2.0","method":"tools/list","id":3}),
            &personal_authority,
        )
        .is_err(),
        "a personal grant cannot reuse the team vault session"
    );
    let refresh = store
        .issue_refresh_token(grant_id, grant.expires_at, current_unix_timestamp())
        .expect("refresh token");
    context
        .named_runtime
        .as_mut()
        .expect("named runtime")
        .vaults
        .remove(&team_id);
    assert!(
        authenticate_mcp_http_request(&context, &request).is_err(),
        "removing a vault from the remote invalidates its existing grants"
    );
    let refresh_params = BTreeMap::from([(
        "refresh_token".to_string(),
        format!("{}.{}", refresh.family_id, refresh.secret.expose()),
    )]);
    assert_eq!(
        handle_local_oauth_refresh(&context, &issuer, &pending.client_id, &refresh_params).status,
        400,
        "a removed vault cannot refresh an existing connection"
    );
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
fn named_runtime_rejects_a_definition_changed_before_listener_startup() {
    let temporary = tempfile::tempdir().expect("temporary state");
    let vault = temporary.path().join("vault");
    std::fs::create_dir_all(&vault).expect("vault");
    let process = DaemonProcessContext {
        registry: vulcan_daemon::registry::WikiRegistry::at(temporary.path().join("daemon.toml")),
        state_root: temporary.path().join("state"),
        verbose: false,
    };
    let wiki_id = vulcan_daemon::registry::WikiId::parse("personal").expect("wiki ID");
    process
        .registry
        .add(
            &vulcan_daemon::registry::AddWikiRequest {
                id: wiki_id.clone(),
                path: vault,
                profile: None,
                groups: Vec::new(),
                git_dir: None,
                permissions_profile: None,
                sync_backend: Some("none".to_string()),
                platform_profile: None,
            },
            false,
        )
        .expect("registration");
    let id = vulcan_daemon::mcp_remote::McpRemoteId::parse("personal-chatgpt").expect("remote ID");
    let stale = process
        .registry
        .add_mcp_remote(
            vulcan_daemon::mcp_remote::AddMcpRemoteRequest {
                id: id.clone(),
                bind: "127.0.0.1:8765".to_string(),
                public_url: "https://mcp.example.test/personal".to_string(),
                authentication: McpRemoteAuthentication::IndieAuth {
                    identity: "https://identity.example.test/alice".to_string(),
                },
                vaults: vec![vulcan_daemon::mcp_remote::McpRemoteVault {
                    wiki_id,
                    ceiling_profile: "readonly".to_string(),
                    default_profile: "readonly".to_string(),
                    tool_packs: vec!["notes-read".to_string()],
                }],
            },
            false,
        )
        .expect("remote");
    process
        .registry
        .update_mcp_remote(
            &id,
            vulcan_daemon::mcp_remote::UpdateMcpRemoteRequest {
                public_url: Some("https://mcp.example.test/changed".to_string()),
                ..Default::default()
            },
            false,
        )
        .expect("changed remote");
    let error = run_named_mcp_remote_inner(&process, &stale, None, None, None)
        .expect_err("stale definition must not start a listener");
    assert!(error.message.contains("changed while starting"));
}

#[cfg(feature = "oauth")]
#[test]
fn resident_mcp_service_groups_instances_and_accepts_multi_vault_definitions() {
    let temporary = tempfile::tempdir().expect("temporary state");
    let process = DaemonProcessContext {
        registry: vulcan_daemon::registry::WikiRegistry::at(temporary.path().join("daemon.toml")),
        state_root: temporary.path().join("state"),
        verbose: false,
    };
    let scheduler =
        Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).expect("scheduler"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let resident_service = |remotes: &[McpRemoteDefinition]| {
        resident_named_mcp_service(
            &process,
            remotes,
            Arc::clone(&scheduler),
            runtime.handle().clone(),
        )
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
    assert!(resident_service(&[]).expect("empty registry").is_none());
    let first = definition("first");
    let second = definition("second");
    let service = resident_service(&[first.clone(), second])
        .expect("two remotes share one supervised service")
        .expect("service");
    assert_eq!(service.definition.id.as_str(), "listener.mcp-remotes");
    assert!(service.definition.required);

    let mut multi = first;
    let mut second_vault = multi.vaults[0].clone();
    second_vault.wiki_id = vulcan_daemon::registry::WikiId::parse("team").expect("wiki ID");
    multi.vaults.push(second_vault);
    assert!(resident_service(&[multi])
        .expect("multi-vault definitions are accepted")
        .is_some());
}

#[cfg(feature = "oauth")]
#[test]
fn failed_resident_startup_cancels_and_joins_every_listener_thread() {
    let stop = Arc::new(ShutdownSignal::new(false));
    let listener_stop = Arc::clone(&stop);
    let exited = Arc::new(AtomicBool::new(false));
    let listener_exited = Arc::clone(&exited);
    let (started_sender, started_receiver) = mpsc::channel();
    let listener = thread::spawn(move || {
        started_sender.send(()).expect("listener started");
        assert!(listener_stop.wait_timeout(Duration::from_secs(5)));
        listener_exited.store(true, Ordering::SeqCst);
    });
    started_receiver.recv().expect("listener startup");
    let panicked = thread::spawn(|| panic!("simulated listener failure"));

    let error = finish_failed_resident_mcp_startup(
        &stop,
        vec![panicked, listener],
        "named MCP listener failed during startup".to_string(),
    );

    assert!(stop.is_cancelled());
    assert!(exited.load(Ordering::SeqCst));
    assert!(error.contains("failed during startup"));
    assert!(error.contains("listener thread panicked"));
}

#[cfg(feature = "oauth")]
#[test]
fn resident_readiness_rejects_a_queued_exit_after_the_last_ready_event() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(ResidentMcpEvent::Ready("first".to_string()))
        .expect("ready event");
    sender
        .send(ResidentMcpEvent::Exited(
            "first".to_string(),
            Err("bind failed".to_string()),
        ))
        .expect("exit event");
    let expected = BTreeSet::from(["first".to_string()]);

    let error = await_resident_mcp_readiness(
        &receiver,
        &expected,
        Instant::now() + Duration::from_secs(1),
    )
    .expect_err("queued exit must prevent host readiness");
    assert!(error.contains("stopped during startup"));
    assert!(error.contains("bind failed"));
}

#[cfg(feature = "oauth")]
#[test]
fn resident_readiness_uses_one_deadline_for_all_listeners() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(ResidentMcpEvent::Ready("first".to_string()))
        .expect("first ready event");
    let expected = BTreeSet::from(["first".to_string(), "second".to_string()]);

    let error = await_resident_mcp_readiness(
        &receiver,
        &expected,
        Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("recent deadline"),
    )
    .expect_err("expired aggregate deadline must not reset for the second listener");
    assert!(error.contains("timed out"));
}

#[cfg(feature = "oauth")]
#[test]
fn resident_readiness_rejects_disconnected_listeners() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(ResidentMcpEvent::Ready("first".to_string()))
        .expect("ready event");
    drop(sender);
    let expected = BTreeSet::from(["first".to_string()]);

    let error = await_resident_mcp_readiness(
        &receiver,
        &expected,
        Instant::now() + Duration::from_secs(1),
    )
    .expect_err("disconnected listener must not publish readiness");
    assert!(error.contains("disconnected"));
}

#[cfg(feature = "oauth")]
#[test]
fn panicking_resident_listener_reports_its_exit_while_other_listeners_remain_connected() {
    let (sender, receiver) = mpsc::channel();
    let other_listener_sender = sender.clone();
    report_resident_mcp_listener_exit(&sender, "first".to_string(), || -> Result<(), CliError> {
        panic!("simulated listener panic");
    });
    let expected = BTreeSet::from(["first".to_string(), "second".to_string()]);

    let error = await_resident_mcp_readiness(
        &receiver,
        &expected,
        Instant::now() + Duration::from_secs(1),
    )
    .expect_err("listener panic must fail readiness even while the channel remains connected");
    assert!(error.contains("listener panicked"));
    drop(other_listener_sender);
}

#[cfg(feature = "oauth")]
#[test]
#[allow(clippy::too_many_lines)] // Builds a real competing hosted request and cancellation race.
fn hosted_mcp_cancelled_while_queued_never_dispatches_a_write() {
    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    vulcan_core::initialize_vulcan_dir(&paths).expect("initialize vault");
    let mut core = McpServerCore::new(
        &paths,
        Some("unrestricted"),
        &[McpToolPackArg::NotesWrite],
        McpToolPackModeArg::Static,
    )
    .expect("MCP core");
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
    let mut http = consent_test_context(&paths, issuer);
    http.oauth = None;
    http.requested_profile = Some("unrestricted".to_string());
    http.tool_pack_args = vec![McpToolPackArg::NotesWrite];
    let inbound = McpHttpRequest {
        method: "POST".to_string(),
        path: "/mcp".to_string(),
        query: String::new(),
        headers: BTreeMap::new(),
        body: Vec::new(),
    };
    let authority = authenticate_mcp_http_request(&http, &inbound).expect("direct authority");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let scheduler =
        Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).expect("scheduler"));
    let grant = core.selection.grant.clone();
    let blocker = ExecutionContext::new(
        ExecutionVaultIdentity::resolve(paths.vault_root(), None, None).expect("vault"),
        ExecutionAuthority::Caller {
            principal_id: "blocker".to_string(),
            credential_id: None,
            permission_ceiling: grant.clone(),
        },
        grant,
        ExecutionIdentity::new("test:blocker"),
        None,
        ExecutionRetryClass::IndeterminateAfterDispatch,
        ExecutionCancellationToken::default(),
        None,
    )
    .expect("blocking context");
    let held = runtime
        .block_on(scheduler.acquire(&blocker, ScheduledOperation::Mutation, |_| Ok(())))
        .expect("hold vault mutation lane");
    let hosted = HostedMcpExecution {
        executor: Arc::new(HostedExecutor::new(
            Arc::clone(&scheduler),
            Arc::new(HostedJobLedger::at(temporary.path().join("operations"))),
        )),
        scheduler,
        runtime: runtime.handle().clone(),
    };
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "note_create",
            "arguments": {"path": "Blocked.md", "body": "must not appear"}
        }
    });
    assert_eq!(
        mcp_scheduled_operation(&payload),
        ScheduledOperation::Mutation
    );
    let ledger = hosted.executor.ledger();
    http.hosted = Some(hosted);
    let outcome = core.process_http_request_with_timeout(
        payload,
        Duration::from_secs(1),
        &http,
        &inbound,
        &authority,
    );
    let operation_id = match outcome {
        Ok(result) => {
            assert!(result.session_stale);
            let response = result.response.expect("timeout response");
            let data = &response["result"]["structuredContent"];
            let operation_id = data["operation_id"].as_str().expect("durable operation ID");
            assert_eq!(
                data["status_path"],
                format!("/mcp/operations/{operation_id}")
            );
            operation_id.to_string()
        }
        Err(response) => {
            let data = &response["error"]["data"];
            assert_eq!(data["dispatched"], false);
            let operation_id = data["operation_id"].as_str().expect("durable operation ID");
            assert_eq!(
                data["status_path"],
                format!("/mcp/operations/{operation_id}")
            );
            operation_id.to_string()
        }
    };
    drop(held);
    let mut record = ledger
        .load(&operation_id)
        .expect("durable operation record");
    for _ in 0..100 {
        if record.state.is_terminal() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
        record = ledger
            .load(&operation_id)
            .expect("durable operation record");
    }
    assert!(record.state.is_terminal());
    assert!(!record.dispatched);
    assert!(!paths.vault_root().join("Blocked.md").exists());
}

#[cfg(feature = "oauth")]
#[test]
#[allow(clippy::too_many_lines)] // Exercises caller, grant, audience, and scope isolation in one fixture.
fn named_mcp_operation_status_is_bound_to_grant_subject_and_audience() {
    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    vulcan_core::initialize_vulcan_dir(&paths).expect("initialize vault");
    let issuer = Arc::new(
        LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://mcp.example.test/mcp".to_string(),
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
    let mut http = consent_test_context(&paths, issuer);
    let remote_id = vulcan_daemon::mcp_remote::McpRemoteId::parse("personal").expect("remote");
    let wiki_id = vulcan_daemon::registry::WikiId::parse("personal").expect("wiki");
    let instance_id = http.instance_id;
    http.named_runtime = Some(NamedMcpRuntime {
        remote_id: remote_id.clone(),
        vaults: BTreeMap::from([(
            wiki_id.clone(),
            NamedMcpVaultRuntime {
                paths: paths.clone(),
                ceiling_profile: "readonly".to_string(),
                default_profile: "readonly".to_string(),
                eligible_tool_packs: vec!["notes-read".to_string()],
            },
        )]),
        authorization_store: McpAuthorizationStore::at(temporary.path().join("state")),
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let scheduler =
        Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).expect("scheduler"));
    let ledger = Arc::new(HostedJobLedger::at(temporary.path().join("operations")));
    http.hosted = Some(HostedMcpExecution {
        executor: Arc::new(HostedExecutor::new(
            Arc::clone(&scheduler),
            Arc::clone(&ledger),
        )),
        scheduler,
        runtime: runtime.handle().clone(),
    });
    let grant_id = Ulid::new();
    let authority = McpSessionAuthority::granted(
        remote_id,
        instance_id,
        grant_id,
        "client-a".to_string(),
        "https://identity.example.test/alice".to_string(),
        wiki_id,
        "https://mcp.example.test/mcp".to_string(),
        "readonly".to_string(),
        vec!["notes-read".to_string()],
        vec!["mcp:tools".to_string()],
        "token-a",
    );
    let core = McpServerCore::new(
        &paths,
        Some("readonly"),
        &[McpToolPackArg::NotesRead],
        McpToolPackModeArg::Static,
    )
    .expect("core");
    let payload = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"note_create","arguments":{"path":"Idea.md"}}
    });
    let execution = http
        .hosted
        .as_ref()
        .expect("hosted")
        .prepare(
            &core,
            &payload,
            &authority,
            ExecutionCancellationToken::default(),
            ExecutionDeadline::after(Duration::from_secs(5)),
        )
        .expect("register operation");
    let operation_id = execution.identity.operation_id;
    let response = handle_named_mcp_operation_status(&http, &authority, &operation_id);
    assert_eq!(response.status, 200);
    assert!(response
        .extra_headers
        .iter()
        .any(|header| header == &("Cache-Control".to_string(), "no-store".to_string())));
    let body: Value = serde_json::from_slice(&response.body).expect("status JSON");
    assert_eq!(body["state"], "queued");
    assert_eq!(body["operation_id"], operation_id);
    assert!(
        !String::from_utf8_lossy(&response.body).contains(&temporary.path().display().to_string())
    );

    let mut other = authority.clone();
    other.grant_id = Some(Ulid::new());
    assert_eq!(
        handle_named_mcp_operation_status(&http, &other, &operation_id).status,
        404
    );
    let mut other = authority.clone();
    other.subject = Some("https://identity.example.test/bob".to_string());
    assert_eq!(
        handle_named_mcp_operation_status(&http, &other, &operation_id).status,
        404
    );
    let mut other = authority.clone();
    other.audience = Some("https://other.example.test/mcp".to_string());
    assert_eq!(
        handle_named_mcp_operation_status(&http, &other, &operation_id).status,
        404
    );
    let mut other = authority;
    other.scopes.clear();
    assert_ne!(
        handle_named_mcp_operation_status(&http, &other, &operation_id).status,
        200
    );
    let request = McpHttpRequest {
        method: "GET".to_string(),
        path: format!("/mcp/operations/{operation_id}"),
        query: String::new(),
        headers: BTreeMap::new(),
        body: Vec::new(),
    };
    assert_eq!(
        named_mcp_operation_id(&http, &request),
        Some(operation_id.as_str())
    );
}

#[cfg(feature = "oauth")]
#[test]
fn timed_out_named_mutation_reports_durable_status_path_and_stale_session() {
    let payload = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"note_create","arguments":{}}
    });
    let result = hosted_mcp_unknown_result(
        &payload,
        "01abcdefghjkmnpqrstvwxyz12",
        "write outcome is not yet known",
        "/mcp",
    );
    assert!(result.session_stale);
    let response = result.response.expect("tool response");
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["status_path"],
        "/mcp/operations/01abcdefghjkmnpqrstvwxyz12"
    );
    assert_eq!(
        response["result"]["structuredContent"]["outcome"],
        "indeterminate"
    );
}

#[cfg(feature = "oauth")]
#[test]
fn hosted_mcp_known_failures_report_dispatch_and_commit_knowledge() {
    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    vulcan_core::initialize_vulcan_dir(&paths).expect("initialize vault");
    let grant = resolve_permission_profile(&paths, Some("readonly"))
        .expect("profile")
        .grant;
    let execution = ExecutionContext::new(
        ExecutionVaultIdentity::resolve(paths.vault_root(), None, None).expect("vault"),
        ExecutionAuthority::Caller {
            principal_id: "alice".to_string(),
            credential_id: Some("grant-a".to_string()),
            permission_ceiling: grant.clone(),
        },
        grant,
        ExecutionIdentity::new("mcp:personal"),
        Some("https://mcp.example.test/mcp".to_string()),
        ExecutionRetryClass::IndeterminateAfterDispatch,
        ExecutionCancellationToken::default(),
        None,
    )
    .expect("execution");
    let payload = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call"});
    let operation_id = execution.identity.operation_id.clone();
    let before = hosted_mcp_execution_error(
        &payload,
        &execution,
        "/mcp",
        HostedExecutionError::BeforeDispatch {
            operation_id: operation_id.clone(),
            detail: "deadline before dispatch".to_string(),
        },
    )
    .expect_err("known pre-dispatch failure");
    assert_eq!(before["error"]["data"]["operation_id"], operation_id);
    assert_eq!(before["error"]["data"]["dispatched"], false);
    assert_eq!(
        before["error"]["data"]["status_path"],
        format!("/mcp/operations/{operation_id}")
    );

    let failed = hosted_mcp_execution_error(
        &payload,
        &execution,
        "/mcp",
        HostedExecutionError::Operation {
            operation_id,
            detail: "failed before commit".to_string(),
            committed: Some(false),
        },
    )
    .expect_err("known uncommitted failure");
    assert_eq!(failed["error"]["data"]["dispatched"], true);
    assert_eq!(failed["error"]["data"]["committed"], false);
}

#[cfg(feature = "oauth")]
#[test]
fn hosted_mcp_scheduling_treats_custom_and_saving_web_tools_as_mutations() {
    let call = |name| {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": {}}
        })
    };
    assert_eq!(
        mcp_scheduled_operation(&call("note_get")),
        ScheduledOperation::Read
    );
    assert_eq!(
        mcp_scheduled_operation(&call("note_create")),
        ScheduledOperation::Mutation
    );
    assert_eq!(
        mcp_scheduled_operation(&call("web_fetch")),
        ScheduledOperation::Mutation
    );
    assert_eq!(
        mcp_scheduled_operation(&call("custom_tool")),
        ScheduledOperation::Mutation
    );
}

#[cfg(feature = "oauth")]
#[test]
fn named_mcp_session_profile_can_narrow_but_never_widen_without_reconsent() {
    let temporary = tempfile::tempdir().expect("temporary vault");
    let paths = VaultPaths::new(temporary.path());
    vulcan_core::initialize_vulcan_dir(&paths).expect("initialize vault");
    fs::write(
        paths.config_file(),
        "[permissions.profiles.agent]\nread = \"all\"\nwrite = \"all\"\n",
    )
    .expect("writable profile");
    let mut core = McpServerCore::new(
        &paths,
        Some("agent"),
        &[McpToolPackArg::NotesWrite],
        McpToolPackModeArg::Static,
    )
    .expect("MCP core");
    assert!(core.guard.check_write_path("Note.md").is_ok());

    fs::write(
        paths.config_file(),
        "[permissions.profiles.agent]\nread = \"all\"\nwrite = \"none\"\n",
    )
    .expect("narrow profile");
    attenuate_mcp_core_profile(&mut core).expect("narrowed profile");
    assert!(core.guard.check_write_path("Note.md").is_err());

    fs::write(
        paths.config_file(),
        "[permissions.profiles.agent]\nread = \"all\"\nwrite = \"all\"\n",
    )
    .expect("widen profile");
    assert!(attenuate_mcp_core_profile(&mut core)
        .expect_err("existing session cannot widen")
        .contains("widened"));
    assert!(core.guard.check_write_path("Note.md").is_err());
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
