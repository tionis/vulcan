use super::{
    apply_template_create, apply_template_creation_trigger, apply_template_insert,
    build_template_list_report, build_template_preview_report, build_template_show_report,
    parse_native_expression, parse_template_var_bindings, parse_templater_tag,
    random_picture_markdown, render_template_request, render_template_request_with_filter,
    template_value_to_string, TemplateCandidate, TemplateCreateRequest, TemplateEngineKind,
    TemplateInsertMode, TemplateInsertRequest, TemplatePreviewRequest, TemplateRenderRequest,
    TemplateRunMode, TemplateSession, TemplateTimestamp, TemplateValue, TrimMode,
};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
#[cfg(feature = "js_runtime")]
use std::io::{Read, Write};
#[cfg(feature = "js_runtime")]
use std::net::TcpListener;
#[cfg(feature = "js_runtime")]
use std::path::Path;
use std::path::PathBuf;
use tempfile::tempdir;
use vulcan_core::permissions::{PathPermission, ResourceSpecifier};
use vulcan_core::Verbosity;
use vulcan_core::{
    resolve_permission_profile, scan_vault, PermissionFilter, ProfilePermissionGuard, ScanMode,
    VaultConfig, VaultPaths,
};

#[test]
fn template_batch_routing_requires_control_read_before_config_observation() {
    let directory = tempdir().unwrap();
    let paths = VaultPaths::new(directory.path());
    fs::create_dir(directory.path().join(".vulcan")).unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"folder:Allowed/**\"] }\nwrite = { allow = [\"folder:Allowed/**\"] }\n").unwrap();
    let changes = [vulcan_core::ordinary_write::OrdinaryWriteChange {
        path: "Allowed/new.md".into(),
        before: None,
        after: Some("New\n".into()),
    }];
    for config in [None, Some("hidden: [invalid")] {
        if let Some(config) = config {
            fs::write(directory.path().join("mdbase.yaml"), config).unwrap();
        }
        let error = super::validate_template_batch(
            &paths,
            &changes,
            &std::collections::BTreeSet::new(),
            Some("scoped"),
        )
        .unwrap_err();
        assert_eq!(error, "permission denied for required mdbase controls");
        assert!(!directory.path().join("Allowed/new.md").exists());
    }
}

#[test]
fn scoped_template_move_rejects_rewrites_outside_its_grant() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    let paths = VaultPaths::new(root);
    fs::create_dir_all(root.join(".vulcan")).expect("config dir");
    fs::create_dir_all(root.join("Allowed")).expect("allowed dir");
    fs::create_dir_all(root.join("Denied")).expect("denied dir");
    fs::write(
        paths.config_file(),
        "[permissions.profiles.agent]\nread = \"all\"\nwrite = { allow = [\"folder:Allowed/**\"] }\nrefactor = { allow = [\"folder:Allowed/**\"] }\n",
    )
    .expect("config");
    fs::write(root.join("Allowed/Source.md"), "# Source\n").expect("source");
    fs::write(root.join("Denied/Backlink.md"), "[[Source]]\n").expect("backlink");
    scan_vault(&paths, ScanMode::Full).expect("scan");
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("agent")).expect("profile"),
    );

    let error = super::guarded_template_move(
        &paths,
        "Allowed/Source.md",
        "Allowed/Moved.md",
        Some(&guard),
    )
    .expect_err("denied backlink rewrite");
    assert!(!error.is_empty());
    assert_eq!(
        fs::read_to_string(root.join("Allowed/Source.md")).expect("source retained"),
        "# Source\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("Denied/Backlink.md")).expect("backlink retained"),
        "[[Source]]\n"
    );
    assert!(!root.join("Allowed/Moved.md").exists());
}

fn staged_move_fixture(template: &str) -> (tempfile::TempDir, VaultPaths) {
    let temporary = tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir_all(root.join(".vulcan/templates")).unwrap();
    fs::create_dir_all(root.join("Projects")).unwrap();
    fs::create_dir_all(root.join("Archive")).unwrap();
    fs::write(root.join(".vulcan/templates/move.md"), template).unwrap();
    fs::write(root.join("Projects/Task.md"), "# Task\n[Peer](./Peer.md)\n").unwrap();
    fs::write(root.join("Projects/Peer.md"), "# Peer\n").unwrap();
    fs::write(root.join("Backlink.md"), "[[Projects/Task]]\n").unwrap();
    let paths = VaultPaths::new(root);
    scan_vault(&paths, ScanMode::Full).unwrap();
    (temporary, paths)
}

fn insert_move_template(
    paths: &VaultPaths,
) -> Result<super::TemplateInsertReport, super::AppError> {
    apply_template_insert(
        paths,
        &TemplateInsertRequest {
            template: "move".into(),
            note: "Projects/Task.md".into(),
            mode: TemplateInsertMode::Append,
            engine: TemplateEngineKind::Templater,
            vars: HashMap::new(),
        },
    )
}

#[test]
fn native_template_move_insert_commits_companions_backlinks_and_identity_together() {
    let (temporary, paths) = staged_move_fixture(
        "<% tp.file.create_new('companion', 'Companion') %><% tp.file.move('Archive/Task') %>Added",
    );
    let id_at = |path: &str| -> String {
        rusqlite::Connection::open(paths.cache_db())
            .unwrap()
            .query_row("SELECT id FROM documents WHERE path = ?1", [path], |row| {
                row.get(0)
            })
            .unwrap()
    };
    let original = id_at("Projects/Task.md");
    insert_move_template(&paths).unwrap();
    assert!(!temporary.path().join("Projects/Task.md").exists());
    let moved = fs::read_to_string(temporary.path().join("Archive/Task.md")).unwrap();
    assert!(moved.contains("[Peer](Peer.md)"), "{moved}");
    assert!(moved.contains("Added"));
    assert_eq!(
        fs::read_to_string(temporary.path().join("Companion.md")).unwrap(),
        "companion"
    );
    assert_eq!(
        fs::read_to_string(temporary.path().join("Backlink.md")).unwrap(),
        "[[Task]]\n"
    );
    assert_eq!(id_at("Archive/Task.md"), original);
    assert!(
        vulcan_core::ordinary_write::inspect_ordinary_write_batch(&paths)
            .unwrap()
            .is_none()
    );
}

#[test]
fn native_template_failure_does_not_publish_its_staged_move_or_companion() {
    let (temporary, paths) = staged_move_fixture(
        "<% tp.file.create_new('companion', 'Companion') %><% tp.file.move('Archive/Task') %><% tp.file.include('Missing') %>",
    );
    assert!(insert_move_template(&paths).is_err());
    assert_eq!(
        fs::read_to_string(temporary.path().join("Projects/Task.md")).unwrap(),
        "# Task\n[Peer](./Peer.md)\n"
    );
    assert_eq!(
        fs::read_to_string(temporary.path().join("Backlink.md")).unwrap(),
        "[[Projects/Task]]\n"
    );
    assert!(!temporary.path().join("Archive/Task.md").exists());
    assert!(!temporary.path().join("Companion.md").exists());
}

#[cfg(unix)]
#[test]
fn template_move_planning_refuses_a_cached_backlink_replaced_by_an_outside_symlink() {
    let (temporary, paths) = staged_move_fixture("<% tp.file.move('Archive/Task') %>Added");
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("Backlink.md"), "[[Projects/Task]]\n").unwrap();
    fs::remove_file(temporary.path().join("Backlink.md")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("Backlink.md"),
        temporary.path().join("Backlink.md"),
    )
    .unwrap();
    assert!(insert_move_template(&paths).is_err());
    assert!(temporary.path().join("Projects/Task.md").is_file());
    assert!(!temporary.path().join("Archive/Task.md").exists());
    assert_eq!(
        fs::read_to_string(outside.path().join("Backlink.md")).unwrap(),
        "[[Projects/Task]]\n"
    );
}

#[test]
fn staged_move_rechecks_source_and_backlinks_before_publication() {
    for edited_path in ["Projects/Task.md", "Backlink.md"] {
        let (temporary, paths) = staged_move_fixture("Added");
        let staged = super::staged_template_creates();
        super::stage_template_create(&staged, &paths, "Companion.md", "companion".into()).unwrap();
        super::stage_template_move(
            &staged,
            &paths,
            "Projects/Task.md",
            "Archive/Task.md",
            Some("# Task\n[Peer](./Peer.md)\n"),
            None,
        )
        .unwrap();
        let expected =
            super::template_path_content(&paths, Some(&staged), "Archive/Task.md").unwrap();
        fs::write(temporary.path().join(edited_path), "external edit").unwrap();
        let error = super::write_template_result_with_staged_creates(
            &paths,
            "Archive/Task.md",
            Some(&expected),
            "final",
            &staged,
            None,
            "test",
        )
        .unwrap_err();
        assert!(error.to_string().contains("changed before apply"));
        assert_eq!(
            fs::read_to_string(temporary.path().join(edited_path)).unwrap(),
            "external edit"
        );
        assert!(!temporary.path().join("Archive/Task.md").exists());
        assert!(!temporary.path().join("Companion.md").exists());
    }
}

#[test]
fn staged_template_move_rechecks_a_narrowed_refactor_profile() {
    let (temporary, paths) = staged_move_fixture("Added");
    fs::write(
        paths.config_file(),
        "[permissions.profiles.agent]\nread = \"all\"\nwrite = \"all\"\nrefactor = \"all\"\n",
    )
    .unwrap();
    let guard = ProfilePermissionGuard::new(
        &paths,
        resolve_permission_profile(&paths, Some("agent")).unwrap(),
    );
    let staged = super::staged_template_creates();
    super::stage_template_move(
        &staged,
        &paths,
        "Projects/Task.md",
        "Archive/Task.md",
        Some("# Task\n[Peer](./Peer.md)\n"),
        Some(&guard),
    )
    .unwrap();
    let expected = super::template_path_content(&paths, Some(&staged), "Archive/Task.md").unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.agent]\nread = \"all\"\nwrite = \"all\"\nrefactor = { allow = [\"folder:Projects/**\"] }\n").unwrap();
    assert!(super::write_template_result_with_staged_creates(
        &paths,
        "Archive/Task.md",
        Some(&expected),
        "final",
        &staged,
        Some("agent"),
        "test",
    )
    .is_err());
    assert!(temporary.path().join("Projects/Task.md").is_file());
    assert!(!temporary.path().join("Archive/Task.md").exists());
    assert_eq!(
        fs::read_to_string(temporary.path().join("Backlink.md")).unwrap(),
        "[[Projects/Task]]\n"
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn js_template_repeated_moves_remain_staged_and_final_render_failure_is_atomic() {
    let (temporary, paths) = staged_move_fixture(
        "<%* await tp.file.create_new('companion', 'Companion'); await tp.file.move('Archive/First'); await tp.file.rename('Final'); if (await tp.file.exists('Projects/Task.md')) throw new Error('old source visible'); if (!(await tp.file.exists('Archive/Final.md'))) throw new Error('new target missing'); throw new Error('render failure'); %>",
    );
    assert!(insert_move_template(&paths).is_err());
    assert!(temporary.path().join("Projects/Task.md").is_file());
    assert!(!temporary.path().join("Archive/First.md").exists());
    assert!(!temporary.path().join("Archive/Final.md").exists());
    assert!(!temporary.path().join("Companion.md").exists());
}

#[cfg(feature = "js_runtime")]
#[test]
fn js_template_repeated_moves_publish_only_the_final_destination() {
    let (temporary, paths) = staged_move_fixture(
        "<%* await tp.file.create_new('companion', 'Companion'); await tp.file.move('Archive/First'); await tp.file.rename('Final'); if (await tp.file.exists('Projects/Task.md')) throw new Error('old source visible'); if (!(await tp.file.exists('Archive/Final.md'))) throw new Error('new target missing'); tR += 'Added'; %>",
    );
    let report = insert_move_template(&paths).unwrap();
    assert!(!temporary.path().join("Projects/Task.md").exists());
    assert!(!temporary.path().join("Archive/First.md").exists());
    assert!(temporary.path().join("Archive/Final.md").is_file());
    assert!(temporary.path().join("Companion.md").is_file());
    assert!(!report
        .changed_paths
        .contains(&"Archive/First.md".to_string()));
    assert_eq!(
        fs::read_to_string(temporary.path().join("Backlink.md")).unwrap(),
        "[[Final]]\n"
    );
}

fn fixed_template_timestamp() -> TemplateTimestamp {
    TemplateTimestamp::from_millis(
        vulcan_core::expression::functions::parse_date_like_string("2026-04-04T09:30:00Z")
            .expect("fixed timestamp should parse"),
    )
}

#[test]
fn parses_template_var_bindings() {
    let vars =
        parse_template_var_bindings(&["project=Vulcan".to_string(), "mood=focused".to_string()])
            .expect("vars should parse");
    assert_eq!(vars["project"], "Vulcan");
    assert_eq!(vars["mood"], "focused");
}

#[test]
fn detects_templater_engine_from_tag_syntax() {
    assert_eq!(
        super::detect_template_engine("<% tp.file.title %>", TemplateEngineKind::Auto),
        TemplateEngineKind::Templater
    );
    assert_eq!(
        super::detect_template_engine("{{title}}", TemplateEngineKind::Auto),
        TemplateEngineKind::Native
    );
}

#[test]
fn parses_templater_tags_with_trim_markers() {
    let (tag, next) = parse_templater_tag("a<%_ tp.file.title -%>b", 1).expect("tag");
    assert_eq!(tag.left_trim, TrimMode::All);
    assert_eq!(tag.right_trim, TrimMode::Newline);
    assert_eq!(tag.body, "tp.file.title");
    assert_eq!(next, 22);
}

#[test]
fn parses_native_path_and_call_expressions() {
    assert_eq!(
        parse_native_expression("tp.frontmatter[\"note type\"]").expect("path"),
        super::NativeExpression::Path(vec![
            super::NativePathPart::Name("tp".to_string()),
            super::NativePathPart::Name("frontmatter".to_string()),
            super::NativePathPart::Index("note type".to_string()),
        ])
    );
    assert!(matches!(
        parse_native_expression("tp.date.now(\"YYYY-MM-DD\", 7)").expect("call"),
        super::NativeExpression::Call { .. }
    ));
}

#[test]
fn renders_arrays_like_templater() {
    assert_eq!(
        template_value_to_string(&TemplateValue::Array(vec![
            TemplateValue::String("a".to_string()),
            TemplateValue::String("b".to_string()),
            TemplateValue::String("c".to_string()),
        ])),
        "a,b,c"
    );
}

#[test]
fn random_picture_supports_optional_size_markdown() {
    assert_eq!(
        random_picture_markdown(Some("200x200"), Some("landscape"), true),
        "![](https://source.unsplash.com/random/200x200?landscape|200x200)"
    );
}

#[test]
fn templater_native_interpolation_reads_file_and_frontmatter_context() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();

    let rendered = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "Title <% tp.file.title %>\nStatus <% tp.frontmatter.status %>\n",
        target_path: "Projects/Alpha.md",
        target_contents: Some("---\nstatus: active\n---\nBody\n"),
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect("template should render");

    assert_eq!(rendered.content, "Title Alpha\nStatus active\n");
}

#[test]
fn templater_date_now_uses_moment_tokens_and_evaluated_reference_args() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();

    // Regression: `dddd` rendered as the day of month twice ("0202") and the
    // `tp.file.title` reference was ignored in favour of the current date.
    let rendered = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "# <% tp.file.title %> - <% tp.date.now(\"dddd\", 0, tp.file.title, \"YYYY-MM-DD\") %>\n<% tp.date.now(\"ddd D MMM [week] YYYY\", 1, tp.file.title, \"YYYY-MM-DD\") %>\n",
        target_path: "Journal/2026-09-02.md",
        target_contents: None,
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect("template should render");

    assert_eq!(
        rendered.content,
        "# 2026-09-02 - Wednesday\nThu 3 Sep week 2026\n"
    );
}

#[test]
fn templater_include_rejects_an_absolute_path() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();

    let error = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "<% tp.file.include(\"/tmp/outside.md\") %>",
        target_path: "Output.md",
        target_contents: None,
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect_err("absolute include must be rejected");
    assert!(error.to_string().contains("doesn't exist"));
}

#[cfg(unix)]
#[test]
fn templater_include_rejects_a_symlink_to_an_outside_file() {
    use std::os::unix::fs::symlink;

    let vault = tempdir().expect("vault dir");
    let outside = tempdir().expect("outside dir");
    let outside_note = outside.path().join("outside.md");
    fs::write(&outside_note, "outside secret").expect("outside note");
    symlink(&outside_note, vault.path().join("Linked.md")).expect("include symlink");
    let paths = VaultPaths::new(vault.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();

    let error = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "<% tp.file.include(\"Linked.md\") %>",
        target_path: "Output.md",
        target_contents: None,
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect_err("symlinked include must be rejected");
    assert!(error.to_string().contains("doesn't exist"));
}

#[test]
fn templater_include_respects_the_active_read_filter() {
    let temp_dir = tempdir().expect("temp dir");
    fs::create_dir_all(temp_dir.path().join("Private")).expect("private dir");
    fs::write(temp_dir.path().join("Private/Secret.md"), "private secret").expect("private note");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();
    let filter = PermissionFilter::new(PathPermission {
        allow: vec![ResourceSpecifier::All],
        deny: vec![ResourceSpecifier::Folder("Private/**".to_string())],
    });

    let error = render_template_request_with_filter(
        TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: "<% tp.file.include(\"Private/Secret.md\") %>",
            target_path: "Output.md",
            target_contents: None,
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Dynamic,
            reference_date: None,
        },
        Some(&filter),
    )
    .expect_err("denied include must be rejected");
    assert!(error.to_string().contains("doesn't exist"));
}

#[test]
fn native_renderer_supports_quickadd_date_and_file_tokens() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();
    let request = TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "",
        target_path: "Projects/Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Native,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Append,
        reference_date: None,
    };
    let mut session = TemplateSession::new(request, TemplateEngineKind::Native, None, None, None);
    session.timestamp = fixed_template_timestamp();
    session.now = session.timestamp;

    let rendered = session
            .render_native_text(
                "{{DATE}} {{DATE:YYYY/MM/DD+3}} {{TIME}} {{TITLE}} {{FILE_NAME}} {{FILE_PATH}} {{LINKCURRENT}}",
            )
            .expect("native quickadd text should render");

    assert_eq!(
        rendered,
        "2026-04-04 2026/04/07 09:30 Alpha Alpha Projects/Alpha.md [[Projects/Alpha]]"
    );
}

#[test]
fn native_renderer_supports_quickadd_value_and_vdate_tokens() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::from([
        ("title".to_string(), "Release Planning".to_string()),
        ("due".to_string(), "tomorrow".to_string()),
        // Keep the test non-interactive even when cargo test inherits a TTY.
        ("owner".to_string(), String::new()),
    ]);
    let request = TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "",
        target_path: "Projects/Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Native,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Append,
        reference_date: None,
    };
    let mut session = TemplateSession::new(request, TemplateEngineKind::Native, None, None, None);
    session.timestamp = fixed_template_timestamp();
    session.now = session.timestamp;

    let rendered = session
            .render_native_text(
                "{{VALUE:title|case:slug}} / {{VALUE:title|case:title}} / {{VALUE:owner|Anonymous}} / {{VDATE:due,YYYY-MM-DD}} / {{VDATE:due,dddd}}",
            )
            .expect("quickadd value tokens should render");

    assert_eq!(
        rendered,
        "release-planning / Release Planning / Anonymous / 2026-04-05 / Sunday"
    );
}

#[test]
fn native_renderer_supports_quickadd_global_variables() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let mut config = VaultConfig::default();
    config.quickadd.global_variables = HashMap::from([
        ("Project".to_string(), "[[Projects/Alpha]]".to_string()),
        (
            "agenda".to_string(),
            "- {{VALUE:title|case:slug}} due {{VDATE:due,YYYY-MM-DD}}".to_string(),
        ),
    ])
    .into_iter()
    .collect();
    let vars = HashMap::from([
        ("title".to_string(), "Release Planning".to_string()),
        ("due".to_string(), "tomorrow".to_string()),
    ]);
    let request = TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "",
        target_path: "Projects/Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Native,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Append,
        reference_date: None,
    };
    let mut session = TemplateSession::new(request, TemplateEngineKind::Native, None, None, None);
    session.timestamp = fixed_template_timestamp();
    session.now = session.timestamp;

    let rendered = session
        .render_native_text(
            "{{GLOBAL_VAR:project}} / {{GLOBAL_VAR:AGENDA}} / {{GLOBAL_VAR:missing}}",
        )
        .expect("quickadd global variables should render");

    assert_eq!(
        rendered,
        "[[Projects/Alpha]] / - release-planning due 2026-04-05 / "
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_js_interpolation_supports_string_methods() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();

    let rendered = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "<% tp.file.title.toUpperCase() %>",
        target_path: "Projects/Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect("template should render");

    assert_eq!(rendered.content, "ALPHA");
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_js_execution_uses_tr_output_accumulator() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();

    let rendered = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "<%* tR += tp.file.title + '-ok'; %>",
        target_path: "Projects/Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect("template should render");

    assert_eq!(rendered.content, "Alpha-ok");
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_loads_user_scripts_from_configured_folder() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(temp_dir.path().join("Scripts")).expect("script dir");
    fs::write(
        temp_dir.path().join("Scripts/echo.js"),
        "module.exports = function (msg) { return `echo:${msg}`; };",
    )
    .expect("script");

    let mut config = VaultConfig::default();
    config.templates.user_scripts_folder = Some(Path::new("Scripts").to_path_buf());
    let vars = HashMap::new();
    let rendered = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[TemplateCandidate {
            name: "example.md".to_string(),
            source: "vulcan",
            display_path: ".vulcan/templates/example.md".to_string(),
            absolute_path: temp_dir.path().join(".vulcan/templates/example.md"),
            warning: None,
        }],
        template_path: None,
        template_text: "<% tp.user.echo(\"Hello\") %>",
        target_path: "Projects/Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect("template should render");

    assert_eq!(rendered.content, "echo:Hello");
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_hooks_run_after_rendering() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();

    let rendered = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text:
                "<%* tp.hooks.on_all_templates_executed(async () => { await tp.file.create_new('Hooked', 'Created'); }); %>Main body",
            target_path: "Projects/Alpha.md",
            target_contents: Some("Body\n"),
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: true,
            run_mode: TemplateRunMode::Dynamic,
            reference_date: None,
        })
        .expect("template should render");

    assert_eq!(rendered.content, "Main body");
    assert!(rendered
        .changed_paths
        .iter()
        .any(|path| path == "Created.md"));
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Created.md")).expect("created note"),
        "Hooked"
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_file_create_waits_for_the_vault_write_lock() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    vulcan_core::initialize_vulcan_dir(&paths).expect("init");
    let lock = vulcan_core::write_lock::acquire_write_lock(&paths).expect("write lock");
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let config = VaultConfig::default();
        let vars = HashMap::new();
        started_tx.send(()).expect("started");
        let result = render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: "<%* await tp.file.create_new('Body', 'Created'); %>",
            target_path: "Main.md",
            target_contents: None,
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: true,
            run_mode: TemplateRunMode::Create,
            reference_date: None,
        });
        done_tx.send(result).expect("result");
    });
    started_rx.recv().expect("worker started");
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
    assert!(!temp_dir.path().join("Created.md").exists());
    drop(lock);

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker finished")
        .expect("template rendered");
    worker.join().expect("worker join");
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Created.md")).expect("created note"),
        "Body"
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_system_command_functions_expand_internal_templates() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let mut config = VaultConfig::default();
    config.templates.enable_system_commands = true;
    config.templates.templates_pairs = vec![vulcan_core::config::TemplaterCommandPairConfig {
        name: "echo".to_string(),
        command: "echo <% tp.file.title %>".to_string(),
    }];
    let vars = HashMap::new();

    let rendered = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "<% tp.user.echo() %>",
        target_path: "Projects/Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect("template should render");

    assert_eq!(rendered.content.trim(), "Alpha");
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_web_requests_respect_allowlist_and_json_path() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        let body = r#"{"title":"Vulcan"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("response should write");
    });

    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let mut config = VaultConfig::default();
    config.templates.web_allowlist = vec!["127.0.0.1".to_string()];
    let vars = HashMap::new();

    let rendered = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: &format!(
            "<% tp.web.request(\"http://127.0.0.1:{}/data\", \"title\") %>",
            address.port()
        ),
        target_path: "Projects/Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect("template should render");

    assert_eq!(rendered.content, "Vulcan");
}

#[cfg(not(feature = "js_runtime"))]
#[test]
fn templater_web_helpers_emit_diagnostics_without_js_runtime() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();

    let rendered = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "<% tp.web.request(\"https://example.com\") %>",
        target_path: "Projects/Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Dynamic,
        reference_date: None,
    })
    .expect("template should render");

    assert_eq!(rendered.content, "");
    assert_eq!(rendered.diagnostics.len(), 1);
    assert!(rendered.diagnostics[0].contains("js_runtime"));
}

#[test]
fn resolve_template_file_matches_by_bare_name() {
    let candidates = vec![
        TemplateCandidate {
            name: "daily.md".to_string(),
            display_path: ".vulcan/templates/daily.md".to_string(),
            source: "vulcan",
            absolute_path: PathBuf::from(".vulcan/templates/daily.md"),
            warning: None,
        },
        TemplateCandidate {
            name: "weekly.md".to_string(),
            display_path: ".vulcan/templates/weekly.md".to_string(),
            source: "vulcan",
            absolute_path: PathBuf::from(".vulcan/templates/weekly.md"),
            warning: None,
        },
    ];

    let paths = VaultPaths::new(PathBuf::from("/tmp/fake-vault"));
    let result = super::resolve_template_file(&paths, &candidates, "daily")
        .expect("should match by bare name");
    assert_eq!(result.name, "daily.md");
}

#[test]
fn resolve_template_file_matches_by_display_path_with_directory() {
    let candidates = vec![TemplateCandidate {
        name: "daily.md".to_string(),
        display_path: "00-09 Management & Meta/05 Templates/daily.md".to_string(),
        source: "templater",
        absolute_path: PathBuf::from("00-09 Management & Meta/05 Templates/daily.md"),
        warning: None,
    }];

    let paths = VaultPaths::new(PathBuf::from("/tmp/fake-vault"));

    let without_ext = super::resolve_template_file(
        &paths,
        &candidates,
        "00-09 Management & Meta/05 Templates/daily",
    );
    assert!(without_ext.is_ok());

    let with_ext = super::resolve_template_file(
        &paths,
        &candidates,
        "00-09 Management & Meta/05 Templates/daily.md",
    );
    assert!(with_ext.is_ok());

    let by_name = super::resolve_template_file(&paths, &candidates, "daily");
    assert!(by_name.is_ok());
}

#[test]
fn list_templates_in_directory_scans_subdirectories() {
    let tmp = tempdir().expect("tempdir should be created");
    let root = tmp.path();

    let sub = root.join("subdir");
    std::fs::create_dir(&sub).expect("subdir should be created");
    std::fs::write(sub.join("nested.md"), "# Nested").expect("nested template should write");
    std::fs::write(root.join("top.md"), "# Top").expect("top template should write");
    std::fs::write(root.join("ignored.txt"), "ignore me").expect("ignored file should write");
    // The scanner treats any letter case of `.md` as a note.
    std::fs::write(root.join("Upper.MD"), "# Upper").expect("upper template should write");

    let templates = super::list_templates_in_directory(root, "Templates", "test")
        .expect("should list templates");

    assert_eq!(templates.len(), 3);
    assert!(templates.iter().any(|template| template.name == "Upper.MD"));
    let names: Vec<&str> = templates
        .iter()
        .map(|template| template.name.as_str())
        .collect();
    assert!(names.contains(&"nested.md"));
    assert!(names.contains(&"top.md"));

    let nested = templates
        .iter()
        .find(|template| template.name == "nested.md")
        .expect("nested template should be present");
    assert!(nested.display_path.contains("subdir"));
}

#[test]
fn build_template_list_report_lists_vulcan_templates() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(root.join(".vulcan/templates/daily.md"), "# Daily\n").expect("daily template");
    fs::write(root.join(".vulcan/templates/weekly.md"), "# Weekly\n").expect("weekly template");

    let report = build_template_list_report(&VaultPaths::new(root)).expect("list report");
    assert_eq!(report.templates.len(), 2);
    assert_eq!(report.templates[0].name, "daily.md");
    assert_eq!(report.templates[1].name, "weekly.md");
    assert!(report.warnings.is_empty());
}

#[test]
fn build_template_show_report_reads_template_contents() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(root.join(".vulcan/templates/daily.md"), "# Daily\nHello\n").expect("template");

    let report = build_template_show_report(&VaultPaths::new(root), "daily").expect("show report");
    assert_eq!(report.name, "daily.md");
    assert_eq!(report.source, "vulcan");
    assert_eq!(report.path, ".vulcan/templates/daily.md");
    assert_eq!(report.content, "# Daily\nHello\n");
}

#[test]
fn build_template_preview_report_renders_named_template() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(root.join(".vulcan/templates/daily.md"), "# {{title}}\n").expect("template");

    let report = build_template_preview_report(
        &VaultPaths::new(root),
        &TemplatePreviewRequest {
            template: "daily".to_string(),
            output_path: Some("Projects/Alpha".to_string()),
            engine: TemplateEngineKind::Auto,
            vars: HashMap::new(),
        },
    )
    .expect("preview report");

    assert_eq!(report.template, "daily.md");
    assert_eq!(report.template_source, "vulcan");
    assert_eq!(report.path, "Projects/Alpha.md");
    assert_eq!(report.engine, "native");
    assert_eq!(report.content, "# Alpha\n");
}

#[test]
fn direct_template_reads_refuse_pending_ordinary_write_journal() {
    #[derive(Serialize)]
    struct JournalFixture<'a> {
        version: u32,
        transaction_id: &'a str,
        changes: Vec<vulcan_core::ordinary_write::OrdinaryWriteChange>,
        digest: String,
    }

    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    let paths = VaultPaths::new(root);
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(root.join(".vulcan/templates/daily.md"), "# {{title}}\n").expect("template");
    fs::write(root.join("Inbox.md"), "Original\n").expect("note");
    let directory = paths
        .operational_state_dir()
        .expect("operational state")
        .join("ordinary-write");
    fs::create_dir_all(&directory).expect("journal directory");
    let mut journal = JournalFixture {
        version: 1,
        transaction_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        changes: vec![vulcan_core::ordinary_write::OrdinaryWriteChange {
            path: "Inbox.md".to_string(),
            before: Some("Original\n".to_string()),
            after: Some("Updated\n".to_string()),
        }],
        digest: String::new(),
    };
    journal.digest = blake3::hash(&serde_json::to_vec(&journal).expect("journal bytes"))
        .to_hex()
        .to_string();
    let journal_path = directory.join("journal.json");
    fs::write(
        &journal_path,
        serde_json::to_vec(&journal).expect("sealed journal"),
    )
    .expect("pending journal");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600))
            .expect("owner-only journal");
    }

    let preview = TemplatePreviewRequest {
        template: "daily".to_string(),
        output_path: Some("Projects/Alpha".to_string()),
        engine: TemplateEngineKind::Auto,
        vars: HashMap::new(),
    };
    assert_eq!(
        build_template_list_report(&paths)
            .expect_err("list must fail closed")
            .code(),
        Some("ordinary_write_pending")
    );
    assert_eq!(
        build_template_show_report(&paths, "daily")
            .expect_err("show must fail closed")
            .code(),
        Some("ordinary_write_pending")
    );
    assert_eq!(
        build_template_preview_report(&paths, &preview)
            .expect_err("preview must fail closed")
            .code(),
        Some("ordinary_write_pending")
    );
    vulcan_core::ordinary_write::recover_ordinary_write_batch(&paths)
        .expect("recover pending batch")
        .expect("pending batch");
    assert_eq!(
        build_template_preview_report(&paths, &preview)
            .expect("preview after recovery")
            .content,
        "# Alpha\n"
    );
}

#[test]
fn apply_template_create_writes_note_and_reports_changed_paths() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(root.join(".vulcan/templates/daily.md"), "# {{title}}\n").expect("template");

    let report = apply_template_create(
        &VaultPaths::new(root),
        &TemplateCreateRequest {
            template: "daily".to_string(),
            output_path: Some("Projects/Alpha".to_string()),
            engine: TemplateEngineKind::Auto,
            vars: HashMap::new(),
        },
    )
    .expect("create report");

    assert_eq!(report.template, "daily.md");
    assert_eq!(report.path, "Projects/Alpha.md");
    assert_eq!(report.engine, "native");
    assert_eq!(report.changed_paths, vec!["Projects/Alpha.md".to_string()]);
    assert_eq!(
        fs::read_to_string(root.join("Projects/Alpha.md")).expect("created note"),
        "# Alpha\n"
    );
}

#[test]
fn template_create_commits_companion_with_final_note_and_leaves_none_on_collision() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(
        root.join(".vulcan/templates/daily.md"),
        "<% tp.file.create_new('Side body', 'Side') %>Main body",
    )
    .expect("template");
    let paths = VaultPaths::new(root);
    let request = TemplateCreateRequest {
        template: "daily".to_string(),
        output_path: Some("Main".to_string()),
        engine: TemplateEngineKind::Templater,
        vars: HashMap::new(),
    };
    fs::write(root.join("Main.md"), "Existing\n").expect("collision");
    apply_template_create(&paths, &request).expect_err("final collision");
    assert!(!root.join("Side.md").exists());
    assert_eq!(
        fs::read_to_string(root.join("Main.md")).unwrap(),
        "Existing\n"
    );
    fs::remove_file(root.join("Main.md")).expect("clear collision");
    let report = apply_template_create(&paths, &request).expect("create");
    assert_eq!(
        fs::read_to_string(root.join("Side.md")).unwrap(),
        "Side body"
    );
    assert!(fs::read_to_string(root.join("Main.md"))
        .unwrap()
        .ends_with("Main body"));
    assert_eq!(report.changed_paths, vec!["Main.md", "Side.md"]);
}

#[test]
fn template_create_rejects_a_destination_created_while_waiting_for_the_lock() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(root.join(".vulcan/templates/daily.md"), "# {{title}}\n").expect("template");
    let paths = VaultPaths::new(root);
    let lock = vulcan_core::write_lock::acquire_write_lock(&paths).expect("write lock");
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).expect("started");
        done_tx
            .send(apply_template_create(
                &paths,
                &TemplateCreateRequest {
                    template: "daily".to_string(),
                    output_path: Some("Projects/Alpha".to_string()),
                    engine: TemplateEngineKind::Auto,
                    vars: HashMap::new(),
                },
            ))
            .expect("result");
    });
    started_rx.recv().expect("worker started");
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    fs::create_dir_all(root.join("Projects")).expect("project dir");
    fs::write(root.join("Projects/Alpha.md"), "created elsewhere\n").expect("other create");
    drop(lock);

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker finished")
        .expect_err("destination collision must fail");
    worker.join().expect("worker join");
    assert_eq!(
        fs::read_to_string(root.join("Projects/Alpha.md")).expect("other note"),
        "created elsewhere\n"
    );
}

#[test]
fn disabled_template_creation_trigger_does_not_read_the_target() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());

    let report =
        apply_template_creation_trigger(&paths, "Missing.md", None, Verbosity::Quiet, None)
            .expect("disabled trigger should be a no-op");

    assert!(!report.triggered);
    assert!(report.changed_paths.is_empty());
}

#[test]
fn apply_template_creation_trigger_updates_an_externally_created_note() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::create_dir_all(root.join("Projects")).expect("projects dir");
    fs::write(
        root.join(".vulcan/config.toml"),
        r#"[templates]
trigger_on_file_creation = true
trigger_on_file_creation_mode = "regex"
file_templates = [
  { regex = "^Projects/.*\\.md$", template = "project" },
  { regex = ".*", template = "fallback" },
]
ignore_folders_on_creation = [{ folder = "Projects/Archive" }]
"#,
    )
    .expect("config");
    fs::write(root.join(".vulcan/templates/project.md"), "# {{title}}\n").expect("template");
    fs::write(root.join(".vulcan/templates/fallback.md"), "# Fallback\n").expect("fallback");
    fs::write(root.join("Projects/Alpha.md"), "").expect("created note");

    let report = apply_template_creation_trigger(
        &VaultPaths::new(root),
        "Projects/Alpha.md",
        None,
        Verbosity::Quiet,
        None,
    )
    .expect("trigger report");

    assert!(report.triggered);
    assert_eq!(report.template.as_deref(), Some("project"));
    assert_eq!(report.engine.as_deref(), Some("native"));
    assert_eq!(report.changed_paths, ["Projects/Alpha.md"]);
    assert_eq!(
        fs::read_to_string(root.join("Projects/Alpha.md")).expect("rendered note"),
        "# Alpha\n"
    );

    fs::create_dir_all(root.join("Projects/Archive")).expect("archive dir");
    fs::write(root.join("Projects/Archive/Old.md"), "").expect("ignored note");
    let ignored = apply_template_creation_trigger(
        &VaultPaths::new(root),
        "Projects/Archive/Old.md",
        None,
        Verbosity::Quiet,
        None,
    )
    .expect("ignored report");
    assert!(!ignored.triggered);
}

#[test]
fn creation_trigger_rejects_a_concurrent_edit_after_rendering() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(
        root.join(".vulcan/config.toml"),
        "[templates]\ntrigger_on_file_creation = true\ntrigger_on_file_creation_mode = \"regex\"\nfile_templates = [{ regex = \"^Projects/.*\\\\.md$\", template = \"project\" }]\n",
    )
    .expect("config");
    fs::write(root.join(".vulcan/templates/project.md"), "# {{title}}\n").expect("template");
    fs::create_dir_all(root.join("Projects")).expect("projects dir");
    fs::write(root.join("Projects/Alpha.md"), "").expect("source");
    let paths = VaultPaths::new(root);
    let lock = vulcan_core::write_lock::acquire_write_lock(&paths).expect("write lock");
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).expect("started");
        done_tx
            .send(apply_template_creation_trigger(
                &paths,
                "Projects/Alpha.md",
                None,
                Verbosity::Quiet,
                None,
            ))
            .expect("result");
    });
    started_rx.recv().expect("worker started");
    let pending = done_rx.recv_timeout(Duration::from_millis(150));
    assert!(
        pending.is_err(),
        "trigger returned before the lock: {pending:?}"
    );
    fs::write(root.join("Projects/Alpha.md"), "concurrent edit\n").expect("concurrent edit");
    drop(lock);

    let error = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker finished")
        .expect_err("stale trigger must fail");
    worker.join().expect("worker join");
    assert!(error
        .to_string()
        .contains("note changed during note template trigger"));
    assert_eq!(
        fs::read_to_string(root.join("Projects/Alpha.md")).expect("current source"),
        "concurrent edit\n"
    );
}

#[test]
fn creation_trigger_stages_companion_until_existing_note_update_succeeds() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(
        root.join(".vulcan/config.toml"),
        "[templates]\ntrigger_on_file_creation = true\ntrigger_on_file_creation_mode = \"folder\"\nfolder_templates = [{ folder = \"Projects\", template = \"project\" }]\n",
    )
    .expect("config");
    fs::write(
        root.join(".vulcan/templates/project.md"),
        "<% tp.file.create_new('Side body', 'Side') %>Main body",
    )
    .expect("template");
    fs::create_dir_all(root.join("Projects")).expect("projects dir");
    fs::write(root.join("Projects/Alpha.md"), "").expect("source");
    let paths = VaultPaths::new(root);
    let lock = vulcan_core::write_lock::acquire_write_lock(&paths).expect("write lock");
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        done_tx
            .send(apply_template_creation_trigger(
                &paths,
                "Projects/Alpha.md",
                None,
                Verbosity::Quiet,
                None,
            ))
            .expect("result");
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    assert!(!root.join("Side.md").exists());
    fs::write(root.join("Projects/Alpha.md"), "Concurrent edit\n").expect("edit");
    drop(lock);
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker finished")
        .expect_err("stale trigger");
    worker.join().expect("worker join");
    assert!(!root.join("Side.md").exists());
    assert_eq!(
        fs::read_to_string(root.join("Projects/Alpha.md")).unwrap(),
        "Concurrent edit\n"
    );

    fs::write(root.join("Projects/Alpha.md"), "").expect("reset source");
    let report = apply_template_creation_trigger(
        &VaultPaths::new(root),
        "Projects/Alpha.md",
        None,
        Verbosity::Quiet,
        None,
    )
    .expect("trigger");
    assert_eq!(report.changed_paths, vec!["Projects/Alpha.md", "Side.md"]);
    assert_eq!(
        fs::read_to_string(root.join("Side.md")).unwrap(),
        "Side body"
    );
    assert!(fs::read_to_string(root.join("Projects/Alpha.md"))
        .unwrap()
        .ends_with("Main body"));
}

#[cfg(feature = "js_runtime")]
#[test]
fn creation_trigger_writes_to_a_template_moved_target() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::create_dir_all(root.join("Projects")).expect("projects dir");
    fs::write(
        root.join(".vulcan/config.toml"),
        "[templates]\ntrigger_on_file_creation = true\ntrigger_on_file_creation_mode = \"folder\"\nfolder_templates = [{ folder = \"Projects\", template = \"move\" }]\n",
    )
    .expect("config");
    fs::write(
        root.join(".vulcan/templates/move.md"),
        "<%* await tp.file.move('Moved/Alpha'); %>Moved body",
    )
    .expect("template");
    fs::write(root.join("Projects/Alpha.md"), "").expect("source");
    scan_vault(&VaultPaths::new(root), ScanMode::Full).expect("scan");

    let report = apply_template_creation_trigger(
        &VaultPaths::new(root),
        "Projects/Alpha.md",
        None,
        Verbosity::Quiet,
        None,
    )
    .expect("trigger report");
    assert_eq!(report.path, "Moved/Alpha.md");
    assert!(!root.join("Projects/Alpha.md").exists());
    assert_eq!(
        fs::read_to_string(root.join("Moved/Alpha.md")).expect("moved note"),
        "Moved body"
    );
}

#[test]
fn template_creation_trigger_rejects_an_invalid_file_regex() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan")).expect("config dir");
    fs::create_dir_all(root.join("Projects")).expect("projects dir");
    fs::write(
        root.join(".vulcan/config.toml"),
        r#"[templates]
trigger_on_file_creation = true
trigger_on_file_creation_mode = "regex"
file_templates = [{ regex = "[", template = "project" }]
"#,
    )
    .expect("config");
    fs::write(root.join("Projects/Alpha.md"), "").expect("created note");

    let error = apply_template_creation_trigger(
        &VaultPaths::new(root),
        "Projects/Alpha.md",
        None,
        Verbosity::Quiet,
        None,
    )
    .expect_err("invalid regex should fail");

    assert!(error
        .to_string()
        .contains("invalid template file-creation regex `[`"));
}

#[test]
fn apply_template_insert_merges_frontmatter_and_updates_note() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(
        root.join(".vulcan/templates/daily.md"),
        "---\nstatus: backlog\ntags:\n- team\n---\n\n## Template Section\n",
    )
    .expect("template");
    fs::write(
        root.join("Home.md"),
        "---\npriority: high\ntags:\n- release\n---\n\n# Home\n",
    )
    .expect("note");
    scan_vault(&VaultPaths::new(root), ScanMode::Full).expect("scan should succeed");

    let report = apply_template_insert(
        &VaultPaths::new(root),
        &TemplateInsertRequest {
            template: "daily".to_string(),
            note: "Home".to_string(),
            mode: TemplateInsertMode::Prepend,
            engine: TemplateEngineKind::Auto,
            vars: HashMap::new(),
        },
    )
    .expect("insert report");

    assert_eq!(report.template, "daily.md");
    assert_eq!(report.note, "Home.md");
    assert_eq!(report.mode, "prepend");
    assert_eq!(report.engine, "native");
    assert_eq!(report.changed_paths, vec!["Home.md".to_string()]);

    let updated = fs::read_to_string(root.join("Home.md")).expect("updated note");
    assert!(updated.contains("status: backlog"));
    assert!(updated.contains("priority: high"));
    assert!(updated.contains("- release"));
    assert!(updated.contains("- team"));
    assert!(updated.contains("## Template Section"));
    assert!(updated.contains("# Home"));
}

#[test]
fn template_insert_rejects_a_concurrent_note_edit() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(root.join(".vulcan/templates/daily.md"), "Template body\n").expect("template");
    fs::write(root.join("Home.md"), "Original\n").expect("source");
    let paths = VaultPaths::new(root);
    scan_vault(&paths, ScanMode::Full).expect("scan");
    let lock = vulcan_core::write_lock::acquire_write_lock(&paths).expect("write lock");
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).expect("started");
        done_tx
            .send(apply_template_insert(
                &paths,
                &TemplateInsertRequest {
                    template: "daily".to_string(),
                    note: "Home".to_string(),
                    mode: TemplateInsertMode::Append,
                    engine: TemplateEngineKind::Auto,
                    vars: HashMap::new(),
                },
            ))
            .expect("result");
    });
    started_rx.recv().expect("worker started");
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    fs::write(root.join("Home.md"), "Concurrent edit\n").expect("concurrent edit");
    drop(lock);

    let error = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("worker finished")
        .expect_err("stale insert must fail");
    worker.join().expect("worker join");
    assert!(error
        .to_string()
        .contains("note changed during note template insert"));
    assert_eq!(
        fs::read_to_string(root.join("Home.md")).expect("current note"),
        "Concurrent edit\n"
    );
}

#[test]
fn template_insert_does_not_publish_companion_when_final_note_changes() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(
        root.join(".vulcan/templates/daily.md"),
        "<% tp.file.create_new('Side body', 'Side') %>Inserted\n",
    )
    .expect("template");
    fs::write(root.join("Home.md"), "Original\n").expect("source");
    let paths = VaultPaths::new(root);
    scan_vault(&paths, ScanMode::Full).expect("scan");
    let lock = vulcan_core::write_lock::acquire_write_lock(&paths).expect("write lock");
    // Learn when the insert has read and rendered `Home.md`, rather than
    // guessing with a timer: under load the worker may not have read it yet.
    let (rendered_tx, rendered_rx) = mpsc::channel();
    *crate::templates::INSERT_RENDERED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((paths.vault_root().to_path_buf(), rendered_tx));
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        done_tx
            .send(apply_template_insert(
                &paths,
                &TemplateInsertRequest {
                    template: "daily".to_string(),
                    note: "Home".to_string(),
                    mode: TemplateInsertMode::Append,
                    engine: TemplateEngineKind::Templater,
                    vars: HashMap::new(),
                },
            ))
            .expect("result");
    });
    rendered_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the insert rendered the original note");
    // It now waits for the write lock this test holds.
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
    fs::write(root.join("Home.md"), "Concurrent edit\n").expect("concurrent edit");
    drop(lock);
    let outcome = done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("worker finished");
    *crate::templates::INSERT_RENDERED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    outcome.expect_err("stale insert");
    worker.join().expect("worker join");
    assert!(!root.join("Side.md").exists());
    assert_eq!(
        fs::read_to_string(root.join("Home.md")).unwrap(),
        "Concurrent edit\n"
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn template_insert_commits_js_companion_with_final_note() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
    fs::write(
        root.join(".vulcan/templates/side.md"),
        "<%* await tp.file.create_new('Side body', 'Side'); %>Inserted\n",
    )
    .expect("template");
    fs::write(root.join("Home.md"), "Original\n").expect("source");
    let paths = VaultPaths::new(root);
    scan_vault(&paths, ScanMode::Full).expect("scan");
    let report = apply_template_insert(
        &paths,
        &TemplateInsertRequest {
            template: "side".to_string(),
            note: "Home".to_string(),
            mode: TemplateInsertMode::Append,
            engine: TemplateEngineKind::Templater,
            vars: HashMap::new(),
        },
    )
    .expect("insert");
    assert_eq!(report.changed_paths, vec!["Home.md", "Side.md"]);
    assert_eq!(
        fs::read_to_string(root.join("Side.md")).unwrap(),
        "Side body"
    );
    assert!(fs::read_to_string(root.join("Home.md"))
        .unwrap()
        .contains("Inserted\n"));
}

#[test]
fn reference_date_drives_builtin_dates_but_not_tp_date_now() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();
    let request = TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "",
        target_path: "Journal/Daily/2026-03-30.md",
        target_contents: None,
        engine: TemplateEngineKind::Native,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Create,
        reference_date: Some("2026-03-30"),
    };
    let mut session = TemplateSession::new(request, TemplateEngineKind::Native, None, None, None);
    session.now = fixed_template_timestamp();
    session.timestamp = session
        .now
        .on_date("2026-03-30")
        .expect("reference date should parse");

    let builtins = session
        .render_source(
            "{{date}} {{date:dddd}} {{time}} {{DATE+1}}",
            TemplateEngineKind::Native,
            0,
            "test",
        )
        .expect("native template should render");
    let templater = session
        .render_source(
            "<% tp.date.now(\"YYYY-MM-DD\") %> <% tp.file.title %>",
            TemplateEngineKind::Templater,
            0,
            "test",
        )
        .expect("templater template should render");

    assert_eq!(builtins, "2026-03-30 Monday 09:30 2026-03-31");
    assert_eq!(templater, "2026-04-04 2026-03-30");
}

#[test]
fn template_timestamp_moves_time_of_day_onto_another_date() {
    let moved = fixed_template_timestamp()
        .on_date("2025-12-31")
        .expect("date should parse");
    assert_eq!(moved.default_date_string(), "2025-12-31");
    assert_eq!(
        moved.to_millis() - fixed_template_timestamp().to_millis(),
        -94 * 86_400_000
    );
    assert!(fixed_template_timestamp().on_date("not a date").is_none());
}

#[test]
fn template_insert_writes_collection_records_through_managed_writes() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    fs::write(root.join("mdbase.yaml"), "spec_version: \"0.3.0\"\n").unwrap();
    fs::create_dir_all(root.join(".vulcan/templates")).unwrap();
    fs::write(root.join(".vulcan/templates/t.md"), "Inserted\n").unwrap();
    fs::write(root.join("Note.md"), "---\ntitle: Note\n---\nbody\n").unwrap();
    let paths = VaultPaths::new(root);
    scan_vault(&paths, ScanMode::Full).unwrap();
    let report = apply_template_insert(
        &paths,
        &TemplateInsertRequest {
            template: "t".into(),
            note: "Note.md".into(),
            mode: TemplateInsertMode::Append,
            engine: TemplateEngineKind::Native,
            vars: HashMap::new(),
        },
    )
    .expect("a collection record takes the managed write");
    assert_eq!(report.note, "Note.md");
    let written = fs::read_to_string(root.join("Note.md")).unwrap();
    assert!(written.starts_with("---\ntitle: Note\n---\n"), "{written}");
    assert!(written.contains("Inserted"), "{written}");
}

#[cfg(feature = "js_runtime")]
fn render_js_text(
    paths: &VaultPaths,
    config: &VaultConfig,
    text: &str,
    vars: &HashMap<String, String>,
    allow_mutations: bool,
) -> Result<super::TemplateRenderOutput, super::AppError> {
    render_template_request(TemplateRenderRequest {
        paths,
        vault_config: config,
        templates: &[],
        template_path: None,
        template_text: text,
        target_path: "Projects/2026-10-14 Alpha.md",
        target_contents: Some("Body\n"),
        engine: TemplateEngineKind::Templater,
        vars,
        allow_mutations,
        run_mode: TemplateRunMode::Create,
        reference_date: None,
    })
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_control_flow_and_declarations_span_tags() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let rendered = render_js_text(
        &paths,
        &config,
        "<%* const items = ['a', 'b']; -%>\n<%* for (const item of items) { -%>\n- <% item %>\n<%* } -%>\n<%* if (tp.file.title.startsWith('2026')) { -%>\ndated\n<%* } else { -%>\nundated\n<%* } -%>\n",
        &HashMap::new(),
        false,
    )
    .expect("template should render");
    assert_eq!(rendered.content, "- a\n- b\ndated\n");
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_awaits_real_promises_without_rewriting_source() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let rendered = render_js_text(
        &paths,
        &config,
        "<% await new Promise((resolve) => resolve('settled')) %> <%* tR += 'please await your turn' %>",
        &HashMap::new(),
        false,
    )
    .expect("template should render");
    assert_eq!(rendered.content, "settled please await your turn");
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_script_errors_name_the_template_line_and_message() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let error = render_js_text(
        &paths,
        &config,
        "intro\n<%* const x = undefinedThing.foo; %>\nafter\n",
        &HashMap::new(),
        false,
    )
    .expect_err("undefined global should fail");
    assert_eq!(error.code(), Some("template_script_error"));
    assert_eq!(
        error.message(),
        "template `inline template` failed at line 2: ReferenceError: undefinedThing is not defined\n  2 | <%* const x = undefinedThing.foo; %>"
    );

    let error = render_js_text(
        &paths,
        &config,
        "a\nb\n<%*\nconst ok = 1;\nconst broken = ;\n%>",
        &HashMap::new(),
        false,
    )
    .expect_err("syntax error should fail");
    assert!(
        error.message().starts_with(
            "template `inline template` failed at line 5: SyntaxError: unexpected token in expression: ';'"
        ),
        "{}",
        error.message()
    );

    let error = render_js_text(
        &paths,
        &config,
        "<%* throw 'plain string' %>",
        &HashMap::new(),
        false,
    )
    .expect_err("thrown string should fail");
    assert!(error.message().contains("uncaught value: plain string"));
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_host_errors_name_the_failing_api() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let error = render_js_text(
        &paths,
        &config,
        "<%* tR += tp.file.include('[[Missing]]') %>",
        &HashMap::new(),
        false,
    )
    .expect_err("missing include should fail");
    assert!(
        error
            .message()
            .contains("failed at line 1: tp.file.include(): File [[Missing]] doesn't exist"),
        "{}",
        error.message()
    );
    let error = render_js_text(
        &paths,
        &config,
        "<% tp.file.include('[[Missing]]') %>",
        &HashMap::new(),
        false,
    )
    .expect_err("missing native include should fail");
    assert!(
        error
            .message()
            .contains("failed at line 1: tp.file.include(): File [[Missing]] doesn't exist"),
        "{}",
        error.message()
    );
}

#[test]
fn unterminated_templater_tags_report_their_line() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();
    let error = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "ok\n<% tp.file.title",
        target_path: "Alpha.md",
        target_contents: None,
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Create,
        reference_date: None,
    })
    .expect_err("unterminated tag should fail");
    assert_eq!(error.code(), Some("template_syntax_error"));
    assert!(error
        .message()
        .starts_with("template `inline template` failed at line 2: unterminated Templater tag"));
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_scripts_time_out_with_a_clear_error() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let mut config = VaultConfig::default();
    config.js_runtime.default_timeout_seconds = 1;
    let error = render_js_text(
        &paths,
        &config,
        "<%* while (true) {} %>",
        &HashMap::new(),
        false,
    )
    .expect_err("infinite loop should time out");
    assert_eq!(error.code(), Some("template_script_timeout"));
    assert!(error.message().contains("did not finish within 1s"));
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_user_script_errors_point_at_the_script() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(temp_dir.path().join("Scripts")).expect("script dir");
    fs::write(
        temp_dir.path().join("Scripts/helper.js"),
        "function helper() {\n  return missingValue + 1;\n}\nmodule.exports = helper;\n",
    )
    .expect("script");
    fs::write(
        temp_dir.path().join("Scripts/broken.js"),
        "module.exports = ;\n",
    )
    .expect("script");
    let mut config = VaultConfig::default();
    config.templates.user_scripts_folder = Some(PathBuf::from("Scripts"));

    let error = render_js_text(
        &paths,
        &config,
        "<% tp.user.helper() %>",
        &HashMap::new(),
        false,
    )
    .expect_err("broken script should fail to load");
    assert!(
        error
            .message()
            .contains("failed to load Templater user script `Scripts/broken.js` (line 1)"),
        "{}",
        error.message()
    );

    fs::remove_file(temp_dir.path().join("Scripts/broken.js")).expect("remove");
    let error = render_js_text(
        &paths,
        &config,
        "<% tp.user.helper() %>",
        &HashMap::new(),
        false,
    )
    .expect_err("script error should fail");
    assert!(
        error.message().contains(
            "failed at line 1: ReferenceError: missingValue is not defined (in helper (Scripts/helper.js))"
        ),
        "{}",
        error.message()
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_js_include_renders_the_included_template_and_frontmatter() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    fs::write(
        temp_dir.path().join("Partial.md"),
        "---\nkind: partial\n---\n<%* const n = 2 * 3; %>six=<% n %>",
    )
    .expect("partial");
    let config = VaultConfig::default();
    let rendered = render_js_text(
        &paths,
        &config,
        "---\ntitle: main\n---\n<%* const inc = await tp.file.include('[[Partial]]'); tR += inc.trim(); %>",
        &HashMap::new(),
        false,
    )
    .expect("template should render");
    assert_eq!(
        rendered.content,
        "---\ntitle: main\nkind: partial\n---\nsix=6"
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_js_create_new_renders_a_found_template_for_the_new_note() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    fs::create_dir_all(temp_dir.path().join("Templates")).expect("templates");
    fs::write(
        temp_dir.path().join("Templates/Child.md"),
        "child of <% tp.file.title %>",
    )
    .expect("template");
    let config = VaultConfig::default();
    let rendered = render_js_text(
        &paths,
        &config,
        "<%* await tp.file.create_new(tp.file.find_tfile('Templates/Child'), 'Kid', false, 'Notes'); %>done",
        &HashMap::new(),
        true,
    )
    .expect("template should render");
    assert_eq!(rendered.content, "done");
    assert_eq!(
        fs::read_to_string(temp_dir.path().join("Notes/Kid.md")).expect("created"),
        "child of Kid"
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_js_file_lookups_respect_the_read_filter() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join("Secret")).expect("secret dir");
    fs::write(root.join("Secret/Hidden.md"), "hidden\n").expect("hidden");
    let paths = VaultPaths::new(root);
    let filter = PermissionFilter::new(PathPermission {
        allow: vec![ResourceSpecifier::All],
        deny: vec![ResourceSpecifier::Folder("Secret/**".to_string())],
    });
    let config = VaultConfig::default();
    let vars = HashMap::new();
    let rendered = render_template_request_with_filter(
        TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: "<% tp.file.exists('Secret/Hidden') %>/<% tp.file.find_tfile('Secret/Hidden') === null %>",
            target_path: "Projects/Alpha.md",
            target_contents: None,
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Create,
            reference_date: None,
        },
        Some(&filter),
    )
    .expect("template should render");
    assert_eq!(rendered.content, "false/true");
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_hook_failures_are_reported() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let error = render_js_text(
        &paths,
        &config,
        "body\n<%* tp.hooks.on_all_templates_executed(() => {\n  throw new Error('hook broke');\n}); %>",
        &HashMap::new(),
        false,
    )
    .expect_err("hook failure should surface");
    assert!(
        error.message().contains("failed at line 3: hook broke"),
        "{}",
        error.message()
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_suggester_returns_the_chosen_item_and_validates_vars() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let template = "<%* const pick = await tp.system.suggester((item) => item.label, [{ label: 'Meeting', id: 1 }, { label: 'Idea', id: 2 }], false, 'Note type'); tR += pick.id; %>";
    let vars = HashMap::from([("note_type".to_string(), "Idea".to_string())]);
    let rendered = render_js_text(&paths, &config, template, &vars, false).expect("render");
    assert_eq!(rendered.content, "2");

    let vars = HashMap::from([("note_type".to_string(), "Task".to_string())]);
    let error = render_js_text(&paths, &config, template, &vars, false).expect_err("bad var");
    assert!(
        error.message().contains(
            "failed at line 1: --var note_type=\"Task\" matches none of the options of tp.system.suggester(\"Note type\"); choose one of: \"Meeting\", \"Idea\""
        ),
        "{}",
        error.message()
    );

    let error = render_js_text(&paths, &config, template, &HashMap::new(), false)
        .expect_err("unanswered suggester");
    assert_eq!(error.code(), Some(super::TEMPLATE_INPUT_REQUIRED_CODE));
    assert!(
        error.message().contains(
            "tp.system.suggester(\"Note type\") needs a choice: pass --var note_type=<option>"
        ),
        "{}",
        error.message()
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_multi_suggester_and_prompt_answers() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::from([
        ("tags".to_string(), "b, c".to_string()),
        ("Project name".to_string(), "Vulcan".to_string()),
    ]);
    let rendered = render_js_text(
        &paths,
        &config,
        "<% (await tp.system.multi_suggester(['A', 'B', 'C'], ['a', 'b', 'c'], false, 'Tags')).join('+') %> <% tp.system.prompt('Project name') %> <% tp.system.prompt('Owner', 'me') %>",
        &vars,
        false,
    )
    .expect("render");
    assert_eq!(rendered.content, "b+c Vulcan me");
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_prompts_use_an_installed_interactive_prompter() {
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Answers(RefCell<Vec<String>>);
    impl super::TemplatePrompter for Answers {
        fn prompt(&self, request: &super::TemplatePromptRequest) -> Result<Option<String>, String> {
            self.0
                .borrow_mut()
                .push(format!("prompt:{}", request.message));
            Ok(Some("typed".to_string()))
        }
        fn suggest(
            &self,
            request: &super::TemplateSuggestRequest,
        ) -> Result<Option<Vec<usize>>, String> {
            self.0
                .borrow_mut()
                .push(format!("suggest:{}", request.options.join("|")));
            Ok(None)
        }
    }

    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let answers = Rc::new(Answers(RefCell::new(Vec::new())));
    let rendered = super::with_template_prompter(answers.clone(), || {
        render_js_text(
            &paths,
            &config,
            "<% tp.system.prompt('Name') %>/<% (await tp.system.suggester(['x', 'y'], [1, 2])) ?? 'cancelled' %>",
            &HashMap::new(),
            false,
        )
    })
    .expect("render");
    assert_eq!(rendered.content, "typed/cancelled");
    assert_eq!(*answers.0.borrow(), ["prompt:Name", "suggest:x|y"]);
}

#[test]
fn native_suggester_reads_options_from_frontmatter() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::from([("suggester_1".to_string(), "beta".to_string())]);
    let rendered = render_template_request(TemplateRenderRequest {
        paths: &paths,
        vault_config: &config,
        templates: &[],
        template_path: None,
        template_text: "<% tp.system.suggester(tp.frontmatter.choices, tp.frontmatter.choices) %>",
        target_path: "Alpha.md",
        target_contents: Some("---\nchoices: [alpha, beta]\n---\n"),
        engine: TemplateEngineKind::Templater,
        vars: &vars,
        allow_mutations: false,
        run_mode: TemplateRunMode::Create,
        reference_date: None,
    })
    .expect("render");
    assert_eq!(rendered.content, "beta");
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_exposes_moment_like_obsidian() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let rendered = render_js_text(
        &paths,
        &config,
        "<% moment(tp.file.title.slice(0, 10), 'YYYY-MM-DD').add(1, 'day').format('YYYY-MM-DD dddd') %>|<% tp.obsidian.moment === moment && window.moment === moment %>",
        &HashMap::new(),
        false,
    )
    .expect("render");
    assert_eq!(rendered.content, "2026-10-15 Thursday|true");
}

#[test]
fn templater_file_path_and_folder_follow_templater_semantics() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let vars = HashMap::new();
    let render = |text: &str| {
        render_template_request(TemplateRenderRequest {
            paths: &paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: text,
            target_path: "Projects/Sub/Note.md",
            target_contents: None,
            engine: TemplateEngineKind::Templater,
            vars: &vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Create,
            reference_date: None,
        })
        .expect("render")
        .content
    };
    let absolute = temp_dir
        .path()
        .join("Projects/Sub/Note.md")
        .display()
        .to_string();
    // Native interpreter and JS runtime agree.
    for suffix in ["", " + ''"] {
        assert_eq!(
            render(&format!("<% tp.file.path(true){suffix} %>")),
            "Projects/Sub/Note.md"
        );
        assert_eq!(render(&format!("<% tp.file.path(){suffix} %>")), absolute);
        assert_eq!(render(&format!("<% tp.file.folder(){suffix} %>")), "Sub");
        assert_eq!(
            render(&format!("<% tp.file.folder(true){suffix} %>")),
            "Projects/Sub"
        );
    }
}

#[test]
fn templater_include_extracts_heading_and_block_subpaths() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    fs::write(
        temp_dir.path().join("Source.md"),
        "# Intro\nhello\n## Details\nmore\n### Deeper\ndeep\n# Other\nlast line ^tail\n",
    )
    .expect("source");
    let config = VaultConfig::default();
    let vars = HashMap::new();
    for (link, expected) in [
        ("[[Source#Details]]", "## Details\nmore\n### Deeper\ndeep\n"),
        ("[[Source#^tail]]", "last line"),
    ] {
        for template in [
            format!("<% tp.file.include('{link}') %>"),
            format!("<%* tR += await tp.file.include('{link}') %>"),
        ] {
            let rendered = render_template_request(TemplateRenderRequest {
                paths: &paths,
                vault_config: &config,
                templates: &[],
                template_path: None,
                template_text: &template,
                target_path: "Out.md",
                target_contents: None,
                engine: TemplateEngineKind::Templater,
                vars: &vars,
                allow_mutations: false,
                run_mode: TemplateRunMode::Create,
                reference_date: None,
            })
            .expect("render");
            assert_eq!(rendered.content, expected, "{template}");
        }
    }
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_app_shim_reads_the_vault_and_rejects_unknown_apis() {
    let temp_dir = tempdir().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan")).expect("vulcan dir");
    fs::create_dir_all(root.join("Notes")).expect("notes");
    fs::write(
        root.join("Notes/Src.md"),
        "---\nstatus: open\n---\n# Heading\n#tag [[Other]]\n",
    )
    .expect("note");
    let paths = VaultPaths::new(root);
    scan_vault(&paths, ScanMode::Full).expect("scan");
    let config = VaultConfig::default();
    let rendered = render_js_text(
        &paths,
        &config,
        "<%* const file = app.vault.getAbstractFileByPath('Notes/Src.md'); const cache = app.metadataCache.getFileCache(file); tR += [app.vault.getMarkdownFiles().map((f) => f.path).join(','), file.parent.name, cache.frontmatter.status, cache.tags[0].tag, cache.headings[0].heading, cache.links[0].link, (await app.vault.read(file)).length > 0, app.workspace.getActiveFile().basename].join('|'); %>",
        &HashMap::new(),
        false,
    )
    .expect("render");
    assert_eq!(
        rendered.content,
        "Notes/Src.md|Notes|open|#tag|Heading|Other|true|2026-10-14 Alpha"
    );

    let error = render_js_text(
        &paths,
        &config,
        "<% app.commands.executeCommandById('x') %>",
        &HashMap::new(),
        false,
    )
    .expect_err("unsupported api");
    assert!(error
        .message()
        .contains("app.commands is not available in Vulcan templates"));
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_process_front_matter_updates_the_rendered_note_in_hooks() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let rendered = render_js_text(
        &paths,
        &config,
        "---\nstatus: draft\n---\nBody<%* tp.hooks.on_all_templates_executed(async () => { await app.fileManager.processFrontMatter(tp.config.target_file, (fm) => { fm.status = 'ready'; }); }); %>",
        &HashMap::new(),
        false,
    )
    .expect("render");
    assert_eq!(rendered.content, "---\nstatus: ready\n---\nBody");

    let error = render_js_text(
        &paths,
        &config,
        "<%* await app.fileManager.processFrontMatter(tp.config.target_file, (fm) => { fm.x = 1; }); %>",
        &HashMap::new(),
        false,
    )
    .expect_err("outside hooks");
    assert!(error
        .message()
        .contains("must run inside tp.hooks.on_all_templates_executed()"));
}

#[cfg(feature = "js_runtime")]
#[test]
fn templater_obsidian_helpers_cover_yaml_and_notices() {
    let temp_dir = tempdir().expect("temp dir");
    let paths = VaultPaths::new(temp_dir.path());
    let config = VaultConfig::default();
    let rendered = render_js_text(
        &paths,
        &config,
        "<% tp.obsidian.parseYaml('a: [1, 2]').a[1] %>|<% tp.obsidian.stringifyYaml({ b: 'c' }).trim() %><%* new tp.obsidian.Notice('Saved'); %>|<% tp.file.selection() %>",
        &HashMap::new(),
        false,
    )
    .expect("render");
    assert_eq!(rendered.content, "2|b: c|");
    assert!(rendered.diagnostics.iter().any(|d| d == "Notice: Saved"));
}
