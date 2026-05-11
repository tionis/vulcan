use super::{
    apply_right_trim, apply_template_create, apply_template_insert, build_template_list_report,
    build_template_preview_report, build_template_show_report, parse_native_expression,
    parse_template_var_bindings, parse_templater_tag, random_picture_markdown,
    render_template_request, template_value_to_string, TemplateCandidate, TemplateCreateRequest,
    TemplateEngineKind, TemplateInsertMode, TemplateInsertRequest, TemplatePreviewRequest,
    TemplateRenderRequest, TemplateRunMode, TemplateSession, TemplateTimestamp, TemplateValue,
    TrimMode,
};
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
use vulcan_core::{scan_vault, ScanMode, VaultConfig, VaultPaths};

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
fn trims_one_newline_after_tag() {
    let source = "<% tp.file.title -%>\nBody";
    let cursor = apply_right_trim(source, 20, TrimMode::Newline);
    assert_eq!(&source[cursor..], "Body");
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
    })
    .expect("template should render");

    assert_eq!(rendered.content, "Title Alpha\nStatus active\n");
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
    };
    let mut session = TemplateSession::new(request, TemplateEngineKind::Native);
    session.timestamp = fixed_template_timestamp();

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
    };
    let mut session = TemplateSession::new(request, TemplateEngineKind::Native);
    session.timestamp = fixed_template_timestamp();

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
    };
    let mut session = TemplateSession::new(request, TemplateEngineKind::Native);
    session.timestamp = fixed_template_timestamp();

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

    let templates = super::list_templates_in_directory(root, "Templates", "test")
        .expect("should list templates");

    assert_eq!(templates.len(), 2);
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
