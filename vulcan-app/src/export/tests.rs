
use super::{
    apply_export_profile_create, apply_export_profile_delete, apply_export_profile_rule_add,
    apply_export_profile_rule_delete, apply_export_profile_rule_move,
    apply_export_profile_rule_update, apply_export_profile_set, build_content_transform_rules,
    build_epub_nav_nodes, build_epub_tag_targets, build_export_profile_list,
    build_export_profile_rule_list, build_export_profile_show_report,
    collect_export_attachment_paths, execute_export_query, inject_epub_heading_ids,
    load_export_links, load_exported_notes, prepare_export_data, render_csv_export_payload,
    render_epub_nav_document, render_json_export_payload, render_markdown_export_payload,
    rewrite_epub_link_destination, write_epub_export, write_sqlite_export, write_zip_export,
    BoolConfigUpdate, ConfigValueUpdate, EpubChapter, EpubExportOptions, EpubHeading,
    EpubRenderCallbacks, ExportLinkRecord, ExportProfileCreateRequest, ExportProfileFormat,
    ExportProfileRuleMoveRequest, ExportProfileRuleRequest, ExportProfileRuleWriteAction,
    ExportProfileSetRequest, ExportProfileWriteAction, ExportedNoteDocument,
};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use tempfile::tempdir;
use vulcan_core::config::ExportEpubTocStyleConfig;
use vulcan_core::properties::NoteTaskRecord;
use vulcan_core::{
    scan_vault, EvaluatedInlineExpression, NoteRecord, QueryAst, QueryProjection, QueryReport,
    QuerySource, ScanMode, VaultPaths,
};
use zip::ZipArchive;

fn export_paths() -> (tempfile::TempDir, VaultPaths) {
    let temp_dir = tempdir().expect("temp dir");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("vault root");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir");
    let paths = VaultPaths::new(&vault_root);
    (temp_dir, paths)
}

fn create_json_profile_request() -> ExportProfileCreateRequest {
    ExportProfileCreateRequest {
        format: ExportProfileFormat::Json,
        query: Some("from notes".to_string()),
        query_json: None,
        path: PathBuf::from("exports/public.json"),
        site_profile: None,
        title: None,
        author: None,
        toc: None,
        backlinks: false,
        frontmatter: false,
        pretty: true,
        graph_format: None,
    }
}

fn config_contents(path: &Path) -> String {
    fs::read_to_string(path.join(".vulcan/config.toml")).expect("config contents")
}

fn build_export_transform_vault() -> (tempfile::TempDir, VaultPaths) {
    let temp_dir = tempdir().expect("temp dir");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir");
    fs::create_dir_all(vault_root.join("People")).expect("people dir");
    fs::create_dir_all(vault_root.join("assets")).expect("assets dir");
    fs::write(
        vault_root.join("Home.md"),
        concat!(
            "# Home\n\n",
            "Visible note.\n\n",
            "> [!secret gm]- Internal\n",
            "> Hidden [[People/Bob]].\n",
            "> ![[assets/secret.png]]\n\n",
            "![[assets/public.png]]\n",
        ),
    )
    .expect("home note");
    fs::write(vault_root.join("People/Bob.md"), "# Bob\n").expect("bob note");
    fs::write(vault_root.join("assets/public.png"), b"public").expect("public asset");
    fs::write(vault_root.join("assets/secret.png"), b"secret").expect("secret asset");
    let paths = VaultPaths::new(&vault_root);
    scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
    (temp_dir, paths)
}

fn test_epub_render_inline_value(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), ToOwned::to_owned)
}

#[test]
fn export_profile_create_list_and_show_reports_share_app_layer_logic() {
    let (_temp_dir, paths) = export_paths();
    let report = apply_export_profile_create(
        &paths,
        "public_json",
        &create_json_profile_request(),
        false,
        false,
    )
    .expect("create profile");

    assert_eq!(report.action, ExportProfileWriteAction::Created);
    assert_eq!(
        report.changed_paths,
        vec![
            ".vulcan/config.toml".to_string(),
            ".vulcan/.gitignore".to_string()
        ]
    );
    assert!(report
        .rendered_toml
        .contains("[export.profiles.public_json]"));

    let listed = build_export_profile_list(&paths);
    let expected_resolved = paths
        .vault_root()
        .join("exports/public.json")
        .display()
        .to_string();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "public_json");
    assert_eq!(listed[0].format.as_deref(), Some("json"));
    assert_eq!(listed[0].path.as_deref(), Some("exports/public.json"));
    assert_eq!(
        listed[0].resolved_path.as_deref(),
        Some(expected_resolved.as_str())
    );

    let show = build_export_profile_show_report(&paths, "public_json").expect("show report");
    assert_eq!(show.profile["pretty"], Value::Bool(true));
    assert!(show.rendered_toml.contains("pretty = true"));
    assert!(config_contents(paths.vault_root()).contains("format = \"json\""));
}

#[test]
fn export_profile_set_rewrites_profile_fields_in_shared_config() {
    let (_temp_dir, paths) = export_paths();
    apply_export_profile_create(
        &paths,
        "docs",
        &ExportProfileCreateRequest {
            format: ExportProfileFormat::Markdown,
            query: Some("from notes".to_string()),
            query_json: None,
            path: PathBuf::from("exports/docs.md"),
            site_profile: None,
            title: Some("Docs".to_string()),
            author: None,
            toc: None,
            backlinks: false,
            frontmatter: false,
            pretty: false,
            graph_format: None,
        },
        false,
        false,
    )
    .expect("create markdown profile");

    let report = apply_export_profile_set(
        &paths,
        "docs",
        &ExportProfileSetRequest {
            format: Some(ExportProfileFormat::Json),
            query: None,
            query_json: Some("{\"source\":\"notes\"}".to_string()),
            clear_query: false,
            path: ConfigValueUpdate::Set(PathBuf::from("exports/docs.json")),
            site_profile: ConfigValueUpdate::Keep,
            title: ConfigValueUpdate::Clear,
            author: ConfigValueUpdate::Keep,
            toc: ConfigValueUpdate::Keep,
            backlinks: BoolConfigUpdate::Keep,
            frontmatter: BoolConfigUpdate::Keep,
            pretty: BoolConfigUpdate::SetTrue,
            graph_format: ConfigValueUpdate::Keep,
        },
        false,
    )
    .expect("set profile");

    assert_eq!(report.action, ExportProfileWriteAction::Updated);
    assert_eq!(
        report.changed_paths,
        vec![".vulcan/config.toml".to_string()]
    );
    assert_eq!(report.profile["format"], "json");
    assert!(report.profile["query"].is_null());
    assert_eq!(report.profile["query_json"], "{\"source\":\"notes\"}");
    assert!(report.profile["title"].is_null());
    assert_eq!(report.profile["pretty"], Value::Bool(true));
    let contents = config_contents(paths.vault_root());
    assert!(contents.contains("format = \"json\""));
    assert!(contents.contains("query_json = "));
    assert!(!contents.contains("title = \"Docs\""));
}

#[test]
fn export_profile_create_and_set_support_frontend_bundle_site_profiles() {
    let (_temp_dir, paths) = export_paths();
    let create = apply_export_profile_create(
        &paths,
        "public_bundle",
        &ExportProfileCreateRequest {
            format: ExportProfileFormat::FrontendBundle,
            query: None,
            query_json: None,
            path: PathBuf::from("exports/public-bundle"),
            site_profile: Some("public".to_string()),
            title: None,
            author: None,
            toc: None,
            backlinks: false,
            frontmatter: false,
            pretty: true,
            graph_format: None,
        },
        false,
        false,
    )
    .expect("create frontend bundle profile");
    assert_eq!(create.profile["format"], "frontend-bundle");
    assert_eq!(create.profile["site_profile"], "public");

    let update = apply_export_profile_set(
        &paths,
        "public_bundle",
        &ExportProfileSetRequest {
            format: None,
            query: None,
            query_json: None,
            clear_query: false,
            path: ConfigValueUpdate::Keep,
            site_profile: ConfigValueUpdate::Set("docs".to_string()),
            title: ConfigValueUpdate::Keep,
            author: ConfigValueUpdate::Keep,
            toc: ConfigValueUpdate::Keep,
            backlinks: BoolConfigUpdate::Keep,
            frontmatter: BoolConfigUpdate::Keep,
            pretty: BoolConfigUpdate::Keep,
            graph_format: ConfigValueUpdate::Keep,
        },
        false,
    )
    .expect("update frontend bundle profile");
    assert_eq!(update.profile["site_profile"], "docs");
    assert!(config_contents(paths.vault_root()).contains("site_profile = \"docs\""));
}

#[test]
fn export_profile_rule_workflows_persist_add_update_move_and_delete() {
    let (_temp_dir, paths) = export_paths();
    apply_export_profile_create(
        &paths,
        "public_json",
        &create_json_profile_request(),
        false,
        false,
    )
    .expect("create profile");

    let add_first = apply_export_profile_rule_add(
        &paths,
        "public_json",
        None,
        &ExportProfileRuleRequest {
            query: None,
            query_json: None,
            exclude_callouts: Vec::new(),
            exclude_headings: Vec::new(),
            exclude_frontmatter_keys: Vec::new(),
            exclude_inline_fields: Vec::new(),
            replacement_rules: vec![
                "literal".to_string(),
                "[[People/Bob]]".to_string(),
                "[[People/Alice]]".to_string(),
            ],
        },
        false,
    )
    .expect("add first rule");
    assert_eq!(add_first.action, ExportProfileRuleWriteAction::Added);
    assert_eq!(add_first.rule_index, Some(1));

    let add_second = apply_export_profile_rule_add(
        &paths,
        "public_json",
        None,
        &ExportProfileRuleRequest {
            query: None,
            query_json: None,
            exclude_callouts: vec!["secret".to_string()],
            exclude_headings: Vec::new(),
            exclude_frontmatter_keys: Vec::new(),
            exclude_inline_fields: Vec::new(),
            replacement_rules: Vec::new(),
        },
        false,
    )
    .expect("add second rule");
    assert_eq!(add_second.rule_index, Some(2));

    let update = apply_export_profile_rule_update(
        &paths,
        "public_json",
        1,
        &ExportProfileRuleRequest {
            query: None,
            query_json: None,
            exclude_callouts: Vec::new(),
            exclude_headings: Vec::new(),
            exclude_frontmatter_keys: Vec::new(),
            exclude_inline_fields: Vec::new(),
            replacement_rules: vec![
                "regex".to_string(),
                "[A-Z]+".to_string(),
                "redacted".to_string(),
            ],
        },
        false,
    )
    .expect("update first rule");
    assert_eq!(update.action, ExportProfileRuleWriteAction::Updated);
    assert_eq!(update.rule_index, Some(1));

    let moved = apply_export_profile_rule_move(
        &paths,
        "public_json",
        ExportProfileRuleMoveRequest {
            index: 2,
            before: Some(1),
            after: None,
            last: false,
        },
        false,
    )
    .expect("move rule");
    assert_eq!(moved.action, ExportProfileRuleWriteAction::Moved);
    assert_eq!(moved.previous_rule_index, Some(2));
    assert_eq!(moved.rule_index, Some(1));

    let listed = build_export_profile_rule_list(&paths, "public_json").expect("rule list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].rule["exclude_callouts"][0], "secret");

    let deleted =
        apply_export_profile_rule_delete(&paths, "public_json", 1, false).expect("delete");
    assert_eq!(deleted.action, ExportProfileRuleWriteAction::Deleted);
    assert_eq!(deleted.rule_index, Some(1));

    let remaining = build_export_profile_rule_list(&paths, "public_json").expect("rule list");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].rule["replace"][0]["pattern"], "[A-Z]+");
    assert!(config_contents(paths.vault_root()).contains("regex = true"));
}

#[test]
fn export_profile_delete_and_invalid_regex_rules_are_reported() {
    let (_temp_dir, paths) = export_paths();
    apply_export_profile_create(
        &paths,
        "public_json",
        &create_json_profile_request(),
        false,
        false,
    )
    .expect("create profile");

    let error = build_content_transform_rules(
        &[],
        &[],
        &[],
        &[],
        &["regex".to_string(), "(".to_string(), "x".to_string()],
    )
    .expect_err("invalid regex should fail");
    assert!(error
        .message()
        .contains("content transform replacement rule 1 has invalid regex pattern"));

    let delete = apply_export_profile_delete(&paths, "public_json", false).expect("delete");
    assert!(delete.deleted);
    assert_eq!(
        delete.changed_paths,
        vec![".vulcan/config.toml".to_string()]
    );
    assert!(!config_contents(paths.vault_root()).contains("public_json"));
}

#[test]
fn prepare_export_data_applies_transforms_and_backlink_adjustments() {
    let (_temp_dir, paths) = build_export_transform_vault();
    let report = execute_export_query(
        &paths,
        Some(r#"from notes where file.path matches "^(Home|People/Bob)\.md$""#),
        None,
        None,
    )
    .expect("query report");
    let raw_notes = load_exported_notes(&paths, &report).expect("raw notes");
    let raw_links = load_export_links(&paths, &raw_notes).expect("raw links");
    let transform_rules =
        build_content_transform_rules(&["secret gm".to_string()], &[], &[], &[], &[])
            .expect("rules");
    let prepared = prepare_export_data(&paths, &report, None, transform_rules.as_deref())
        .expect("prepared export");

    let home = prepared
        .notes
        .iter()
        .find(|note| note.note.document_path == "Home.md")
        .expect("home note");
    let bob = prepared
        .notes
        .iter()
        .find(|note| note.note.document_path == "People/Bob.md")
        .expect("bob note");

    assert!(home.content.contains("assets/public.png"));
    assert!(!home.content.contains("assets/secret.png"));
    assert!(!home.content.contains("Hidden [[People/Bob]]"));
    assert_eq!(
        collect_export_attachment_paths(&raw_links),
        vec![
            "assets/public.png".to_string(),
            "assets/secret.png".to_string()
        ]
    );
    assert_eq!(
        collect_export_attachment_paths(&prepared.links),
        vec!["assets/public.png".to_string()]
    );
    assert!(!bob.note.inlinks.iter().any(|link| link == "[[Home]]"));
}

#[test]
fn shared_export_renderers_emit_expected_json_markdown_and_csv() {
    let (_temp_dir, paths) = build_export_transform_vault();
    let report = execute_export_query(
        &paths,
        Some(r#"from notes where file.path matches "^(Home|People/Bob)\.md$""#),
        None,
        None,
    )
    .expect("query report");
    let transform_rules =
        build_content_transform_rules(&["secret gm".to_string()], &[], &[], &[], &[])
            .expect("rules");
    let prepared = prepare_export_data(&paths, &report, None, transform_rules.as_deref())
        .expect("prepared export");

    let json = render_json_export_payload(&report, &prepared.notes, true).expect("json export");
    let parsed: Value = serde_json::from_str(&json).expect("json payload");
    assert_eq!(parsed["result_count"], Value::Number(2.into()));
    assert!(!json.contains("assets/secret.png"));

    let markdown = render_markdown_export_payload(&prepared.notes, Some("Public Notes"));
    assert!(markdown.starts_with("# Public Notes"));
    assert!(!markdown.contains("assets/secret.png"));
    assert!(markdown.contains("assets/public.png"));

    let csv = render_csv_export_payload(&report).expect("csv export");
    assert!(csv.starts_with(
            "document_path,file_name,file_ext,file_mtime,tags,starred,properties,inline_expressions,query"
        ));
    assert!(csv.contains("Home.md"));
    assert!(csv.contains("People/Bob.md"));
}

#[test]
fn write_zip_export_packages_transformed_notes_and_manifest() {
    let (_temp_dir, paths) = build_export_transform_vault();
    let report = execute_export_query(
        &paths,
        Some(r#"from notes where file.path matches "^(Home|People/Bob)\.md$""#),
        None,
        None,
    )
    .expect("query report");
    let transform_rules =
        build_content_transform_rules(&["secret gm".to_string()], &[], &[], &[], &[])
            .expect("rules");
    let prepared = prepare_export_data(&paths, &report, None, transform_rules.as_deref())
        .expect("prepared export");
    let output_path = paths.vault_root().join("exports/public.zip");

    let summary = write_zip_export(
        &paths,
        &output_path,
        &report,
        &prepared.notes,
        &prepared.links,
    )
    .expect("zip export");

    assert_eq!(summary.result_count, 2);
    assert_eq!(summary.attachment_count, 1);

    let file = fs::File::open(&output_path).expect("zip file");
    let mut archive = ZipArchive::new(file).expect("zip archive");
    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .expect("zip entry")
                .name()
                .to_string(),
        );
    }

    assert!(names.contains(&"Home.md".to_string()));
    assert!(names.contains(&"People/Bob.md".to_string()));
    assert!(names.contains(&"assets/public.png".to_string()));
    assert!(!names.contains(&"assets/secret.png".to_string()));
    assert!(names.contains(&".vulcan-export/manifest.json".to_string()));
    assert!(names.contains(&".vulcan-export/notes.json".to_string()));

    let mut manifest = String::new();
    archive
        .by_name(".vulcan-export/manifest.json")
        .expect("manifest")
        .read_to_string(&mut manifest)
        .expect("manifest read");
    assert!(manifest.contains("assets/public.png"));
    assert!(!manifest.contains("assets/secret.png"));

    let mut notes_json = String::new();
    archive
        .by_name(".vulcan-export/notes.json")
        .expect("notes json")
        .read_to_string(&mut notes_json)
        .expect("notes json read");
    assert!(notes_json.contains("assets/public.png"));
    assert!(!notes_json.contains("assets/secret.png"));
}

#[test]
fn write_epub_export_packages_book_navigation_assets_and_backlinks() {
    let (_temp_dir, paths) = build_export_transform_vault();
    let report = execute_export_query(
        &paths,
        Some(r#"from notes where file.path matches "^(Home|People/Bob)\.md$""#),
        None,
        None,
    )
    .expect("query report");
    let prepared = prepare_export_data(&paths, &report, None, None).expect("prepared export");
    let output_path = paths.vault_root().join("exports/public.epub");
    let render_dataview_block = |_: &VaultPaths, _: &str, _: &str, _: &str| String::new();
    let render_base_embed = |_: &VaultPaths, _: &str, _: Option<&str>| String::new();

    let summary = write_epub_export(
        &paths,
        &output_path,
        &prepared.notes,
        &prepared.links,
        EpubExportOptions {
            title: Some("Public Notes"),
            author: Some("Vulcan"),
            backlinks: true,
            frontmatter: false,
            toc_style: ExportEpubTocStyleConfig::Tree,
        },
        EpubRenderCallbacks {
            render_dataview_block: &render_dataview_block,
            render_base_embed: &render_base_embed,
            render_inline_value: &test_epub_render_inline_value,
        },
    )
    .expect("epub export");

    assert_eq!(summary.result_count, 2);

    let file = fs::File::open(&output_path).expect("epub file");
    let mut archive = ZipArchive::new(file).expect("epub archive");

    let mut mimetype = String::new();
    archive
        .by_name("mimetype")
        .expect("mimetype")
        .read_to_string(&mut mimetype)
        .expect("mimetype read");
    assert_eq!(mimetype, "application/epub+zip");

    let mut nav = String::new();
    archive
        .by_name("OEBPS/nav.xhtml")
        .expect("nav")
        .read_to_string(&mut nav)
        .expect("nav read");
    assert!(nav.contains("Public Notes"));
    assert!(nav.contains("Home"));
    assert!(nav.contains("Bob"));

    let mut names = Vec::new();
    for index in 0..archive.len() {
        names.push(
            archive
                .by_index(index)
                .expect("archive entry")
                .name()
                .to_string(),
        );
    }
    assert_eq!(
        names
            .iter()
            .filter(|name| name.starts_with("OEBPS/media/asset-"))
            .count(),
        2
    );

    let mut chapter_by_note = std::collections::HashMap::new();
    for name in names
        .iter()
        .filter(|name| name.starts_with("OEBPS/text/chapter-"))
    {
        let mut chapter = String::new();
        archive
            .by_name(name)
            .expect("chapter")
            .read_to_string(&mut chapter)
            .expect("chapter read");
        if chapter.contains("Home.md") {
            chapter_by_note.insert("Home.md", chapter);
        } else if chapter.contains("People/Bob.md") {
            chapter_by_note.insert("People/Bob.md", chapter);
        }
    }

    let home_chapter = chapter_by_note
        .get("Home.md")
        .expect("home chapter should be captured");
    let bob_chapter = chapter_by_note
        .get("People/Bob.md")
        .expect("bob chapter should be captured");

    assert!(home_chapter.contains("asset-embed asset-embed-image"));
    assert!(home_chapter.contains("src=\"../media/asset-"));
    assert!(bob_chapter.contains("<section class=\"backlinks\">"));
    assert!(bob_chapter.contains(">Home</a>"));
}

#[test]
fn rewrite_epub_link_destination_maps_selected_notes_and_fragments() {
    let note_targets = HashMap::from([
        (
            "Projects/Alpha".to_string(),
            "chapter-001.xhtml".to_string(),
        ),
        (
            "Projects/Alpha.md".to_string(),
            "chapter-001.xhtml".to_string(),
        ),
        ("Home".to_string(), "chapter-002.xhtml".to_string()),
    ]);
    let asset_targets = HashMap::from([(
        "assets/logo.png".to_string(),
        "../media/asset-001.png".to_string(),
    )]);

    assert_eq!(
        rewrite_epub_link_destination(
            "People/Bob.md",
            "Projects/Alpha#Status",
            &note_targets,
            &asset_targets,
        ),
        Some("chapter-001.xhtml#status".to_string())
    );
    assert_eq!(
        rewrite_epub_link_destination("People/Bob.md", "Home", &note_targets, &asset_targets),
        Some("chapter-002.xhtml".to_string())
    );
    assert_eq!(
        rewrite_epub_link_destination(
            "Notes/Guide.md",
            "../assets/logo.png",
            &note_targets,
            &asset_targets,
        ),
        Some("../media/asset-001.png".to_string())
    );
    assert_eq!(
        rewrite_epub_link_destination(
            "Home.md",
            "https://example.com",
            &note_targets,
            &asset_targets,
        ),
        None
    );
}

#[test]
fn inject_epub_heading_ids_applies_unique_anchor_ids_in_order() {
    let html = "<h1>Home</h1><p>x</p><h2>Status</h2><h2>Status</h2>";
    let headings = vec![
        EpubHeading {
            level: 1,
            text: "Home".to_string(),
            anchor_id: "home".to_string(),
        },
        EpubHeading {
            level: 2,
            text: "Status".to_string(),
            anchor_id: "status".to_string(),
        },
        EpubHeading {
            level: 2,
            text: "Status".to_string(),
            anchor_id: "status-2".to_string(),
        },
    ];

    let rendered = inject_epub_heading_ids(html, &headings);

    assert!(rendered.contains("<h1 id=\"home\">Home</h1>"));
    assert!(rendered.contains("<h2 id=\"status\">Status</h2>"));
    assert!(rendered.contains("<h2 id=\"status-2\">Status</h2>"));
}

#[test]
fn epub_tag_targets_are_slugged_and_unique() {
    let notes = vec![
        ExportedNoteDocument {
            note: NoteRecord {
                document_id: "1".to_string(),
                document_path: "Home.md".to_string(),
                file_name: "Home".to_string(),
                file_ext: "md".to_string(),
                file_mtime: 0,
                file_ctime: 0,
                file_size: 0,
                properties: Value::Object(Map::new()),
                tags: vec!["project".to_string(), "Project".to_string()],
                links: Vec::new(),
                starred: false,
                inlinks: Vec::new(),
                aliases: Vec::new(),
                frontmatter: Value::Null,
                periodic_type: None,
                periodic_date: None,
                list_items: Vec::new(),
                tasks: Vec::new(),
                raw_inline_expressions: Vec::new(),
                inline_expressions: Vec::new(),
            },
            content: String::new(),
        },
        ExportedNoteDocument {
            note: NoteRecord {
                document_id: "2".to_string(),
                document_path: "Nested/Deep.md".to_string(),
                file_name: "Deep".to_string(),
                file_ext: "md".to_string(),
                file_mtime: 0,
                file_ctime: 0,
                file_size: 0,
                properties: Value::Object(Map::new()),
                tags: vec!["project/alpha".to_string()],
                links: Vec::new(),
                starred: false,
                inlinks: Vec::new(),
                aliases: Vec::new(),
                frontmatter: Value::Null,
                periodic_type: None,
                periodic_date: None,
                list_items: Vec::new(),
                tasks: Vec::new(),
                raw_inline_expressions: Vec::new(),
                inline_expressions: Vec::new(),
            },
            content: String::new(),
        },
    ];

    let targets = build_epub_tag_targets(&notes);

    assert_ne!(targets["project"], targets["Project"]);
    assert!(targets["project"].starts_with("tag-project"));
    assert!(targets["Project"].starts_with("tag-project"));
    assert_eq!(targets["project/alpha"], "tag-project-alpha.xhtml");
}

#[test]
fn epub_tree_nav_trims_common_prefix_and_keeps_nested_directories() {
    let chapters = vec![
        EpubChapter {
            document_path: "Guides/Intro.md".to_string(),
            title: "Intro".to_string(),
            nav_path: "text/chapter-001.xhtml".to_string(),
            file_href: "chapter-001.xhtml".to_string(),
            headings: Vec::new(),
            content: String::new(),
        },
        EpubChapter {
            document_path: "Guides/Nested/Deep.md".to_string(),
            title: "Deep".to_string(),
            nav_path: "text/chapter-002.xhtml".to_string(),
            file_href: "chapter-002.xhtml".to_string(),
            headings: Vec::new(),
            content: String::new(),
        },
    ];

    let nav = render_epub_nav_document(
        "Guide Export",
        &build_epub_nav_nodes(&chapters, &[], ExportEpubTocStyleConfig::Tree),
    );

    assert!(!nav.contains("toc-directory-label\">Guides<"));
    assert!(nav.contains("toc-directory-label\">Nested<"));
    assert!(nav.contains("href=\"text/chapter-001.xhtml\">Intro</a>"));
    assert!(nav.contains("href=\"text/chapter-002.xhtml\">Deep</a>"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn write_sqlite_export_writes_expected_schema_and_rows() {
    let temp_dir = tempdir().expect("temp dir");
    let output_path = temp_dir.path().join("export.db");
    let note = NoteRecord {
        document_id: "doc-1".to_string(),
        document_path: "Tasks/Alpha.md".to_string(),
        file_name: "Alpha".to_string(),
        file_ext: "md".to_string(),
        file_mtime: 1_700_000_000,
        file_ctime: 1_700_000_000,
        file_size: 128,
        properties: Value::Object(Map::new()),
        tags: vec!["task".to_string(), "project".to_string()],
        links: vec!["[[Tasks/Beta]]".to_string()],
        starred: false,
        inlinks: Vec::new(),
        aliases: vec!["Alias".to_string()],
        frontmatter: serde_json::json!({"status": "open"}),
        periodic_type: None,
        periodic_date: None,
        list_items: Vec::new(),
        tasks: vec![NoteTaskRecord {
            id: "task-1".to_string(),
            list_item_id: "list-1".to_string(),
            status_char: " ".to_string(),
            status_name: "Todo".to_string(),
            status_type: "TODO".to_string(),
            status_next_symbol: None,
            checked: false,
            completed: false,
            text: "Ship Alpha".to_string(),
            byte_offset: 0,
            parent_task_id: None,
            section_heading: Some("Tasks".to_string()),
            line_number: 3,
            properties: Map::from_iter([(
                "taskSource".to_string(),
                Value::String("inline".to_string()),
            )]),
        }],
        raw_inline_expressions: Vec::new(),
        inline_expressions: vec![EvaluatedInlineExpression {
            expression: "2 + 2".to_string(),
            value: Value::from(4),
            error: None,
        }],
    };
    let report = QueryReport {
        query: QueryAst {
            source: QuerySource::Notes,
            predicates: Vec::new(),
            sort: None,
            projection: QueryProjection::All,
            limit: None,
            offset: 0,
        },
        notes: vec![note.clone()],
    };
    let notes = vec![ExportedNoteDocument {
        note,
        content: "# Alpha\n\n- [ ] Ship Alpha\n".to_string(),
    }];
    let links = vec![ExportLinkRecord {
        source_document_path: "Tasks/Alpha.md".to_string(),
        raw_text: "[[Tasks/Beta]]".to_string(),
        link_kind: "wikilink".to_string(),
        display_text: None,
        target_path_candidate: Some("Tasks/Beta".to_string()),
        target_heading: None,
        target_block: None,
        resolved_target_path: Some("Tasks/Beta.md".to_string()),
        origin_context: "body".to_string(),
        byte_offset: 8,
        resolved_target_extension: Some("md".to_string()),
    }];

    let summary =
        write_sqlite_export(&output_path, &report, &notes, &links).expect("sqlite export");

    assert_eq!(summary.result_count, 1);
    assert_eq!(summary.link_count, 1);
    assert_eq!(summary.tag_count, 2);
    assert_eq!(summary.task_count, 1);

    let connection = rusqlite::Connection::open(&output_path).expect("export db");
    let user_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user version");
    let note_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))
        .expect("notes count");
    let link_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM links", [], |row| row.get(0))
        .expect("links count");
    let tag_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))
        .expect("tags count");
    let task_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .expect("tasks count");
    let meta_result_count: String = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'result_count'",
            [],
            |row| row.get(0),
        )
        .expect("meta result count");

    assert_eq!(user_version, 1);
    assert_eq!(note_count, 1);
    assert_eq!(link_count, 1);
    assert_eq!(tag_count, 2);
    assert_eq!(task_count, 1);
    assert_eq!(meta_result_count, "1");
}
