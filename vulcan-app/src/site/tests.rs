
use super::*;
use std::borrow::Cow;
use std::fs;
use tempfile::TempDir;
use vulcan_core::{scan_vault, ScanMode};

fn copy_fixture_vault(name: &str, destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/vaults")
        .join(name);
    copy_dir_recursive(&source, destination);
    fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
}

fn copy_dir_recursive(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("destination should be created");
    for entry in fs::read_dir(source).expect("source dir should be readable") {
        let entry = entry.expect("dir entry should be readable");
        if entry.file_name() == ".vulcan" {
            continue;
        }
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("file type should read").is_dir() {
            copy_dir_recursive(&path, &target);
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).expect("parent should exist");
            }
            fs::copy(path, target).expect("file should copy");
        }
    }
}

fn scan_fixture(vault_root: &Path) {
    scan_vault(&VaultPaths::new(vault_root), ScanMode::Full).expect("scan should succeed");
}

fn scan_fixture_incremental(vault_root: &Path) {
    scan_vault(&VaultPaths::new(vault_root), ScanMode::Incremental)
        .expect("incremental scan should succeed");
}

fn export_link_record(
    raw_text: &str,
    resolved_target_path: Option<&str>,
    resolved_target_extension: Option<&str>,
) -> ExportLinkRecord {
    ExportLinkRecord {
        source_document_path: "Home.md".to_string(),
        raw_text: raw_text.to_string(),
        link_kind: "embed".to_string(),
        display_text: None,
        target_path_candidate: resolved_target_path.map(ToOwned::to_owned),
        target_heading: None,
        target_block: None,
        resolved_target_path: resolved_target_path.map(ToOwned::to_owned),
        origin_context: "body".to_string(),
        byte_offset: 0,
        resolved_target_extension: resolved_target_extension.map(ToOwned::to_owned),
    }
}

fn snapshot_output_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, current: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(current).expect("directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let path = entry.path();
            if entry
                .file_type()
                .expect("file type should be readable")
                .is_dir()
            {
                visit(root, &path, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("output path should stay under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(relative, fs::read(&path).expect("output file should read"));
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn tree_contains_text(root: &Path, needle: &str) -> bool {
    snapshot_output_tree(root).values().any(|bytes| {
        String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    })
}

fn read_site_text(root: &Path, relative: &str) -> String {
    fs::read_to_string(root.join(relative)).expect("site output should be readable")
}

fn read_site_json(root: &Path, relative: &str) -> Value {
    serde_json::from_str(&read_site_text(root, relative)).expect("site json should parse")
}

fn note_output_path(report: &SiteBuildReport, source_path: &str) -> String {
    report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some(source_path))
        .unwrap_or_else(|| panic!("route should exist for {source_path}"))
        .output_path
        .clone()
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

#[test]
fn resolve_embed_target_prefers_note_routes_and_asset_hrefs() {
    let route_map = HashMap::from([(
        "Notes/Target.md".to_string(),
        SiteRoute {
            kind: "note".to_string(),
            source_path: Some("Notes/Target.md".to_string()),
            title: "Target".to_string(),
            slug: "Notes/Target".to_string(),
            url_path: "/notes/target/".to_string(),
            output_path: "notes/target/index.html".to_string(),
        },
    )]);
    let asset_hrefs = HashMap::from([(
        "images/diagram.png".to_string(),
        "/assets/images/diagram.png".to_string(),
    )]);

    let note_embed = export_link_record("![[Notes/Target]]", Some("Notes/Target.md"), Some("md"));
    let asset_embed = export_link_record(
        "![[images/diagram.png]]",
        Some("images/diagram.png"),
        Some("png"),
    );

    assert_eq!(
        resolve_embed_target(&route_map, &asset_hrefs, &note_embed),
        Some(("Notes/Target.md".to_string(), "/notes/target/".to_string()))
    );
    assert_eq!(
        resolve_embed_target(&route_map, &asset_hrefs, &asset_embed),
        Some((
            "images/diagram.png".to_string(),
            "/assets/images/diagram.png".to_string()
        ))
    );
}

#[test]
fn apply_link_policy_to_source_borrows_when_no_rewrite_is_needed() {
    let published = HashSet::from(["Notes/Target.md".to_string()]);
    let note_link = export_link_record("[[Notes/Target]]", Some("Notes/Target.md"), Some("md"));

    let unchanged = apply_link_policy_to_source(
        "[[Notes/Target]]",
        &[&note_link],
        &published,
        SiteLinkPolicyConfig::Warn,
    );
    assert!(matches!(unchanged, Cow::Borrowed(_)));

    let dropped = apply_link_policy_to_source(
        "[[Notes/Target]]",
        &[&note_link],
        &HashSet::new(),
        SiteLinkPolicyConfig::DropLink,
    );
    assert!(matches!(dropped, Cow::Owned(_)));
}

#[test]
fn frontend_bundle_build_emits_typed_contract_and_shared_manifests_with_site_parity() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let site_report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");
    let bundle_report = build_frontend_bundle(
        &VaultPaths::new(&vault_root),
        &FrontendBundleRequest {
            profile: Some("public".to_string()),
            output_dir: vault_root.join("exports/public-bundle"),
            clean: true,
            dry_run: false,
            pretty: true,
        },
    )
    .expect("bundle build should succeed");

    let site_root = vault_root.join(".vulcan/site/public");
    let bundle_root = vault_root.join("exports/public-bundle");
    let site_routes = read_site_json(&site_root, "assets/route-manifest.json");
    let bundle_routes = read_site_json(&bundle_root, "assets/route-manifest.json");
    let site_search = read_site_json(&site_root, "assets/search-index.json");
    let bundle_search = read_site_json(&bundle_root, "assets/search-index.json");
    let site_graph = read_site_json(&site_root, "assets/graph.json");
    let bundle_graph = read_site_json(&bundle_root, "assets/graph.json");
    let bundle_navigation = read_site_json(&bundle_root, "assets/navigation-tree.json");
    let contract = read_site_json(&bundle_root, "frontend-bundle.json");
    let home_note = read_site_json(&bundle_root, "notes/home/index.json");

    assert_eq!(site_report.routes, bundle_report.routes);
    assert_eq!(site_routes, bundle_routes);
    assert_eq!(site_search, bundle_search);
    assert_eq!(site_graph, bundle_graph);
    assert_eq!(
        bundle_report.contract.contract.name,
        "vulcan_frontend_bundle"
    );
    assert_eq!(bundle_report.contract.profile.name, "public");
    assert_eq!(contract["profile"]["shell"]["default_palette"], "system");
    assert_eq!(contract["profile"]["navigation"]["explorer"], true);
    assert_eq!(contract["profile"]["modules"]["toc"], true);
    assert_eq!(
        contract["artifacts"]["navigation_tree"],
        "assets/navigation-tree.json"
    );
    assert_eq!(
        contract["artifacts"]["schema"],
        "schema/frontend-bundle.schema.json"
    );
    assert_eq!(
        contract["artifacts"]["typescript"],
        "schema/frontend-bundle.d.ts"
    );
    assert!(bundle_root
        .join("schema/frontend-bundle.schema.json")
        .exists());
    assert!(bundle_root.join("schema/frontend-bundle.d.ts").exists());
    assert!(bundle_navigation
        .as_array()
        .is_some_and(|nodes| nodes.iter().any(|node| node["title"] == "Home")));
    assert_eq!(home_note["route"]["url_path"], "/notes/home/");
    assert!(home_note["body_html"]
        .as_str()
        .is_some_and(|html| html.contains("Alpha")));
}

#[test]
fn frontend_bundle_build_is_deterministic_and_prefix_aware() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
deploy_path = "/garden"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let request = FrontendBundleRequest {
        profile: Some("public".to_string()),
        output_dir: vault_root.join("exports/public-bundle"),
        clean: true,
        dry_run: false,
        pretty: true,
    };
    let first = build_frontend_bundle(&VaultPaths::new(&vault_root), &request)
        .expect("first bundle build should succeed");
    let first_tree = snapshot_output_tree(&vault_root.join("exports/public-bundle"));

    let second = build_frontend_bundle(
        &VaultPaths::new(&vault_root),
        &FrontendBundleRequest {
            clean: false,
            ..request.clone()
        },
    )
    .expect("second bundle build should succeed");
    let second_tree = snapshot_output_tree(&vault_root.join("exports/public-bundle"));
    let contract = read_site_json(
        &vault_root.join("exports/public-bundle"),
        "frontend-bundle.json",
    );
    let invalidation = read_site_json(
        &vault_root.join("exports/public-bundle"),
        "assets/invalidation.json",
    );

    assert!(first
        .changed_files
        .iter()
        .any(|path| path == "frontend-bundle.json"));
    assert!(second.changed_files.is_empty());
    assert_eq!(first_tree, second_tree);
    assert_eq!(contract["context"]["deploy_path"], "/garden");
    assert!(contract["notes"].as_array().is_some_and(|notes| notes
        .iter()
        .any(|note| note["route"]["url_path"] == "/garden/notes/home/")));
    assert_eq!(invalidation["changed_files"], serde_json::json!([]));
    assert_eq!(invalidation["changed_routes"], serde_json::json!([]));
}

#[test]
fn site_build_writes_pages_and_manifests() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("attachments", &vault_root);
    fs::write(
        vault_root.join("Home.md"),
        r#"---
title: Welcome
site:
  profiles:
    public:
      slug: home
---

# Dashboard

Main image: ![[assets/diagram.png]]

See [[Notes/Embed Note]].

owner:: Alice

```dataview
table status from "Notes"
```
"#,
    )
    .expect("home note should write");
    fs::write(
        vault_root.join("Notes/Embed Note.md"),
        r"---
tags:
  - publish
---

# Embed Note

Body text.
",
    )
    .expect("embed note should write");
    fs::write(vault_root.join("site.css"), "body { outline: none; }").expect("css should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Published Garden"
home = "Home"
output_dir = ".vulcan/site/public"
base_url = "https://notes.example.com"
extra_css = ["site.css"]
search = true
graph = true
rss = true
include_paths = ["Home", "Notes/Embed Note.md"]
"#,
    )
    .expect("site config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");

    assert_eq!(report.profile, "public");
    assert_eq!(report.note_count, 2);
    assert!(report
        .routes
        .iter()
        .any(|route| route.url_path == "/notes/home/"));
    assert!(vault_root.join(".vulcan/site/public/index.html").exists());
    assert!(vault_root
        .join(".vulcan/site/public/assets/search-index.json")
        .exists());
    assert!(vault_root
        .join(".vulcan/site/public/assets/graph.json")
        .exists());
    assert!(vault_root
        .join(".vulcan/site/public/assets/recent-notes.json")
        .exists());
    assert!(vault_root
        .join(".vulcan/site/public/assets/related-notes.json")
        .exists());
    assert!(vault_root.join(".vulcan/site/public/rss.xml").exists());
    let home_html = fs::read_to_string(vault_root.join(".vulcan/site/public/index.html"))
        .expect("home page should read");
    assert!(home_html.contains("Published Garden"));
    let search_html = fs::read_to_string(vault_root.join(".vulcan/site/public/search/index.html"))
        .expect("search page should read");
    let search_index = read_site_json(
        &vault_root.join(".vulcan/site/public"),
        "assets/search-index.json",
    );
    let graph_html = fs::read_to_string(vault_root.join(".vulcan/site/public/graph/index.html"))
        .expect("graph page should read");
    assert!(search_html.contains("site-search-input"));
    assert_eq!(search_index["version"], 2);
    assert_eq!(search_index["documents"].as_array().map(Vec::len), Some(2));
    assert!(search_index["terms"]["dashboard"]
        .as_array()
        .is_some_and(|postings| !postings.is_empty()));
    assert!(graph_html.contains(r#"data-site-graph-canvas="global""#));
    let note_route = report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some("Notes/Embed Note.md"))
        .expect("embed note route should exist")
        .output_path
        .clone();
    let note_html = fs::read_to_string(vault_root.join(".vulcan/site/public").join(note_route))
        .expect("note page should read");
    assert!(note_html.contains("Embed Note"));
    assert!(note_html.contains("/notes/home/"));
}

#[test]
fn site_build_applies_theme_regions_search_dialog_and_local_graph_shell() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::create_dir_all(vault_root.join(".vulcan/site/themes/reference"))
        .expect("theme dir should exist");
    fs::write(
        vault_root.join(".vulcan/site/themes/reference/theme.css"),
        ".theme-header { letter-spacing: 0.08em; }",
    )
    .expect("theme css should write");
    fs::write(
        vault_root.join(".vulcan/site/themes/reference/theme.js"),
        "window.vulcanThemeLoaded = true;",
    )
    .expect("theme js should write");
    fs::write(
            vault_root.join(".vulcan/site/themes/reference/header.html"),
            "<header class=\"site-header\"><div class=\"theme-header\">{{site_title}} {{nav}} {{search_button}} {{theme_toggle}}</div></header>",
        )
        .expect("theme header should write");
    fs::write(
        vault_root.join(".vulcan/site/themes/reference/toolbar.html"),
        "<div class=\"theme-toolbar\">{{toolbar}}</div>",
    )
    .expect("theme toolbar should write");
    fs::write(
        vault_root.join(".vulcan/site/themes/reference/nav.html"),
        "<a class=\"custom-nav\" href=\"{{home_href}}\">Portal</a>",
    )
    .expect("theme nav should write");
    fs::write(
        vault_root.join(".vulcan/site/themes/reference/left_rail.html"),
        "<div class=\"theme-left\">{{left_rail}}</div>",
    )
    .expect("theme left rail should write");
    fs::write(
        vault_root.join(".vulcan/site/themes/reference/right_rail.html"),
        "<div class=\"theme-right\">{{right_rail}}</div>",
    )
    .expect("theme right rail should write");
    fs::write(
        vault_root.join(".vulcan/site/themes/reference/footer.html"),
        "<footer class=\"site-footer\">Custom footer {{profile_name}}</footer>",
    )
    .expect("theme footer should write");
    fs::write(
        vault_root.join(".vulcan/site/themes/reference/note_before.html"),
        "<div class=\"note-before\">{{current_note_path}}</div>",
    )
    .expect("theme note_before should write");
    fs::write(
        vault_root.join(".vulcan/site/themes/reference/note_after.html"),
        "<div class=\"note-after\">{{page_title}}</div>",
    )
    .expect("theme note_after should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Custom Garden"
home = "Home"
output_dir = ".vulcan/site/public"
theme = "reference"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");

    let output_root = vault_root.join(".vulcan/site/public");
    let home_html = read_site_text(&output_root, "index.html");
    let note_route = report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some("Projects/Alpha.md"))
        .expect("alpha route should exist")
        .output_path
        .clone();
    let note_html = read_site_text(&output_root, &note_route);
    let graph_json = read_site_json(&output_root, "assets/graph.json");

    assert!(home_html.contains("data-site-search-dialog"));
    assert!(home_html.contains("theme-toolbar"));
    assert!(home_html.contains("custom-nav"));
    assert!(home_html.contains("theme-left"));
    assert!(home_html.contains("Custom footer public"));
    assert!(home_html.contains("assets/.vulcan/site/themes/reference/theme.css"));
    assert!(home_html.contains("assets/.vulcan/site/themes/reference/theme.js"));
    assert!(note_html.contains("data-site-local-graph"));
    assert!(note_html.contains(r#"data-site-graph-canvas="local""#));
    assert!(note_html.contains("theme-right"));
    assert!(note_html.contains("note-before"));
    assert!(note_html.contains("Projects/Alpha.md"));
    assert!(note_html.contains("note-after"));
    assert!(output_root
        .join("assets/.vulcan/site/themes/reference/theme.css")
        .exists());
    assert!(output_root
        .join("assets/.vulcan/site/themes/reference/theme.js")
        .exists());
    assert!(graph_json["nodes"]
        .as_array()
        .is_some_and(|nodes| nodes.iter().all(|node| node["title"].as_str().is_some())));
}

#[test]
fn site_build_applies_raw_html_policy_from_profile() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        "# Home\n\n<script>alert('secret')</script>\n\n<div class=\"safe\">Visible</div>\n",
    )
    .expect("home note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "HTML Policy"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md"]
raw_html = "strip"
search = false
graph = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");
    let home_html = read_site_text(&vault_root.join(".vulcan/site/public"), "index.html");

    assert!(!home_html.contains("<script>alert('secret')</script>"));
    assert!(!home_html.contains("class=\"safe\""));
    assert!(report.rendered_notes.iter().any(|note| {
        note.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == "raw_html_stripped")
    }));
}

#[test]
fn site_build_tracks_changed_and_deleted_outputs_without_clean_rebuilds() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Incremental Demo"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = false
graph = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let first = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("first build should succeed");
    assert!(!first.changed_files.is_empty());

    let second = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: false,
            dry_run: false,
        },
    )
    .expect("second build should succeed");
    assert!(
        second.changed_files.is_empty(),
        "unexpected second-build changes: {:?}",
        second.changed_files
    );
    assert!(second.deleted_files.is_empty());

    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Incremental Demo"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md"]
search = false
graph = false
"#,
    )
    .expect("updated config should write");
    scan_vault(&VaultPaths::new(&vault_root), ScanMode::Incremental)
        .expect("incremental scan should succeed");

    let third = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: false,
            dry_run: false,
        },
    )
    .expect("third build should succeed");
    assert!(third
        .deleted_files
        .iter()
        .any(|path| path.contains("notes/projects/alpha/index.html")));
    assert!(!vault_root
        .join(".vulcan/site/public/notes/projects/alpha/index.html")
        .exists());
}

#[test]
fn site_build_incrementally_reuses_unaffected_note_pages() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::create_dir_all(vault_root.join("Misc")).expect("misc dir should exist");
    fs::write(
        vault_root.join("Misc/Gamma.md"),
        "# Gamma\n\nStandalone page.\n",
    )
    .expect("gamma note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Incremental Demo"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md", "People/Bob.md", "Misc/Gamma.md"]
search = false
graph = false
backlinks = false
rss = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let paths = VaultPaths::new(&vault_root);
    let first = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("first build should succeed");
    let profile =
        resolve_site_profile(&paths, Some("public"), None).expect("profile should resolve");
    assert!(
        site_build_state_path(&paths, &profile).exists(),
        "incremental state should be persisted after a successful build"
    );

    let home_route = note_output_path(&first, "Home.md");
    let alpha_route = note_output_path(&first, "Projects/Alpha.md");
    let bob_route = note_output_path(&first, "People/Bob.md");
    let gamma_route = note_output_path(&first, "Misc/Gamma.md");

    fs::write(
        vault_root.join("Misc/Gamma.md"),
        "# Gamma\n\nStandalone page with a changed body.\n",
    )
    .expect("gamma note update should write");
    scan_fixture_incremental(&vault_root);

    let second = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: false,
            dry_run: false,
        },
    )
    .expect("second build should succeed");

    assert!(second.changed_files.iter().any(|path| path == &gamma_route));
    assert!(!second.changed_files.iter().any(|path| path == &home_route));
    assert!(!second.changed_files.iter().any(|path| path == &alpha_route));
    assert!(!second.changed_files.iter().any(|path| path == &bob_route));
}

#[test]
fn site_build_incrementally_rerenders_backlink_dependents() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Backlink Demo"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md", "People/Bob.md"]
search = false
graph = false
backlinks = true
rss = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let paths = VaultPaths::new(&vault_root);
    let first = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("first build should succeed");

    let home_route = note_output_path(&first, "Home.md");
    let alpha_route = note_output_path(&first, "Projects/Alpha.md");
    let bob_route = note_output_path(&first, "People/Bob.md");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        r"---
status: active
tags:
  - project
  - work
---

# Alpha

Owned by [[People/Bob]].

## Status

Alpha now stands on its own.
",
    )
    .expect("alpha update should write");
    scan_fixture_incremental(&vault_root);

    let second = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: false,
            dry_run: false,
        },
    )
    .expect("second build should succeed");

    assert!(second.changed_files.iter().any(|path| path == &alpha_route));
    assert!(
        second.changed_files.iter().any(|path| path == &home_route),
        "home note page should rerender when its backlinks change: {:?}",
        second.changed_files
    );
    assert!(!second.changed_files.iter().any(|path| path == &bob_route));
}

#[test]
fn site_build_incrementally_rerenders_dataview_pages_for_non_published_vault_changes() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Dataview Demo"
output_dir = ".vulcan/site/public"
include_paths = ["Dashboard.md"]
search = false
graph = false
backlinks = false
rss = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let paths = VaultPaths::new(&vault_root);
    let first = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("first build should succeed");
    let dashboard_route = note_output_path(&first, "Dashboard.md");

    fs::write(
            vault_root.join("Projects/Beta.md"),
            r"---
status: backlog
reviewed: true
---

#project
priority:: 5

- [/] Prepare backlog 🗓️ 2026-04-03 ✅ 2026-04-04 ➕ 2026-04-01 🛫 2026-04-02 ⏳ 2026-04-05 🔺 🔁 every week ⛔ ALPHA-1 🆔 BETA-1
",
        )
        .expect("beta update should write");
    scan_fixture_incremental(&vault_root);

    let second = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: false,
            dry_run: false,
        },
    )
    .expect("second build should succeed");

    assert!(
        second
            .changed_files
            .iter()
            .any(|path| path == &dashboard_route),
        "dashboard should rerender when a vault note changes its dataview result: {:?}",
        second.changed_files
    );
}

#[test]
fn site_build_writes_recent_and_related_manifests() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Alpha.md"),
        r"---
tags:
  - shared
  - alpha
---

# Alpha

Alpha body.
",
    )
    .expect("alpha note should write");
    fs::write(
        vault_root.join("Beta.md"),
        r"---
tags:
  - shared
  - beta
---

# Beta

Beta body.
",
    )
    .expect("beta note should write");
    fs::write(
        vault_root.join("Gamma.md"),
        r"---
tags:
  - beta
---

# Gamma

Gamma body.
",
    )
    .expect("gamma note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Related Demo"
output_dir = ".vulcan/site/public"
include_paths = ["Alpha.md", "Beta.md", "Gamma.md"]
search = false
graph = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");
    let output_root = vault_root.join(".vulcan/site/public");
    let recent_manifest = read_site_json(&output_root, "assets/recent-notes.json");
    let related_manifest = read_site_json(&output_root, "assets/related-notes.json");

    let recent_entries = recent_manifest
        .as_array()
        .expect("recent manifest should be an array");
    assert_eq!(recent_entries.len(), 3);
    let mut recent_titles = recent_entries
        .iter()
        .map(|entry| {
            entry["title"]
                .as_str()
                .expect("recent entry title should be a string")
                .to_string()
        })
        .collect::<Vec<_>>();
    recent_titles.sort();
    assert_eq!(recent_titles, vec!["Alpha", "Beta", "Gamma"]);
    assert!(recent_entries.iter().all(|entry| {
        entry["file_mtime"].as_i64().is_some()
            && entry["excerpt"]
                .as_str()
                .is_some_and(|value| !value.trim().is_empty())
    }));

    let alpha_related = related_manifest["/notes/alpha/"]
        .as_array()
        .expect("alpha related entries should be an array");
    assert_eq!(alpha_related.len(), 1);
    assert_eq!(alpha_related[0]["source_path"], "Beta.md");
    assert_eq!(alpha_related[0]["title"], "Beta");
    assert_eq!(alpha_related[0]["url"], "/notes/beta/");
    assert_eq!(
        alpha_related[0]["shared_tags"],
        serde_json::json!(["shared"])
    );

    let beta_related = related_manifest["/notes/beta/"]
        .as_array()
        .expect("beta related entries should be an array");
    assert_eq!(beta_related.len(), 2);
    assert_eq!(beta_related[0]["source_path"], "Alpha.md");
    assert_eq!(
        beta_related[0]["shared_tags"],
        serde_json::json!(["shared"])
    );
    assert_eq!(beta_related[1]["source_path"], "Gamma.md");
    assert_eq!(beta_related[1]["shared_tags"], serde_json::json!(["beta"]));

    let gamma_related = related_manifest["/notes/gamma/"]
        .as_array()
        .expect("gamma related entries should be an array");
    assert_eq!(gamma_related.len(), 1);
    assert_eq!(gamma_related[0]["source_path"], "Beta.md");
    assert_eq!(gamma_related[0]["shared_tags"], serde_json::json!(["beta"]));

    assert!(report
        .files
        .contains(&"assets/recent-notes.json".to_string()));
    assert!(report
        .files
        .contains(&"assets/related-notes.json".to_string()));
}

#[test]
fn site_doctor_reports_collisions_and_unpublished_links() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        r"---
site:
  profiles:
    public:
      slug: shared
---

# Home

See [[Private]].
",
    )
    .expect("home note should write");
    fs::write(
        vault_root.join("Other.md"),
        r"---
site:
  profiles:
    public:
      slug: shared
---

# Other
",
    )
    .expect("other note should write");
    fs::write(
        vault_root.join("Private.md"),
        r"---
tags:
  - private
---

# Private
",
    )
    .expect("private note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
include_paths = ["Home.md", "Other.md", "Private.md"]
exclude_tags = ["private"]
link_policy = "error"
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site_doctor_report(&VaultPaths::new(&vault_root), Some("public"))
        .expect("doctor should succeed");

    assert!(report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == "route_collision"));
    assert!(report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == "unpublished_link_target"));
}

#[test]
fn site_profiles_report_supports_implicit_default() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    scan_fixture(&vault_root);

    let profiles = build_site_profiles_report(&VaultPaths::new(&vault_root))
        .expect("profiles report should succeed");

    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].name, "default");
    assert!(profiles[0].implicit);
    assert!(profiles[0].note_count >= 1);
}

#[test]
fn site_build_is_deterministic_across_repeated_runs() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("basic", &vault_root);
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Public Notes"
output_dir = ".vulcan/site/public"
home = "Home"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let paths = VaultPaths::new(&vault_root);
    build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("first build should succeed");
    let first = snapshot_output_tree(&vault_root.join(".vulcan/site/public"));

    build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("second build should succeed");
    let second = snapshot_output_tree(&vault_root.join(".vulcan/site/public"));

    assert_eq!(first, second);
}

#[test]
fn site_build_keeps_profile_outputs_isolated() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Docs")).expect("docs dir should exist");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        r"# Home

Public landing page.
",
    )
    .expect("home note should write");
    fs::write(
        vault_root.join("Private.md"),
        r"# Private

Secret launch checklist.
",
    )
    .expect("private note should write");
    fs::write(
        vault_root.join("Docs/Intro.md"),
        r"# Intro

Docs-only handbook.
",
    )
    .expect("docs note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Public Notes"
output_dir = ".vulcan/site/public"
home = "Home"
include_paths = ["Home.md"]
search = true
graph = true

[site.profiles.docs]
title = "Project Docs"
output_dir = ".vulcan/site/docs"
include_paths = ["Docs/Intro.md"]
search = true
graph = true
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);
    let paths = VaultPaths::new(&vault_root);

    let public = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("public build should succeed");
    let docs = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("docs".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("docs build should succeed");

    assert_eq!(public.note_count, 1);
    assert_eq!(docs.note_count, 1);
    let public_root = vault_root.join(".vulcan/site/public");
    let docs_root = vault_root.join(".vulcan/site/docs");
    assert!(public_root.join("notes/home/index.html").exists());
    assert!(docs_root.join("notes/docs/intro/index.html").exists());
    assert!(!public_root.join("notes/docs/intro/index.html").exists());
    assert!(!docs_root.join("notes/home/index.html").exists());
    assert!(!tree_contains_text(&public_root, "Docs-only handbook"));
    assert!(!tree_contains_text(&public_root, "Secret launch checklist"));
    assert!(!tree_contains_text(&docs_root, "Public landing page"));
    assert!(!tree_contains_text(&docs_root, "Secret launch checklist"));
}

#[test]
fn site_build_applies_frontmatter_metadata_overrides_and_logo_assets() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("attachments", &vault_root);
    fs::write(
        vault_root.join("Home.md"),
        r"---
site:
  profiles:
    public:
      slug: home
      title: Launch Page
      description: Custom publish summary.
      canonical_url: https://notes.example.com/start/
      summary_image: site/social.png
---

# Home

Body text for the published page.
",
    )
    .expect("home note should write");
    fs::create_dir_all(vault_root.join("site")).expect("site dir should exist");
    fs::write(vault_root.join("site/social.png"), b"social").expect("social image should write");
    fs::write(vault_root.join("site/logo.png"), b"logo").expect("logo image should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Published Garden"
base_url = "https://notes.example.com"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md"]
logo = "site/logo.png"
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");

    let note_route = report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some("Home.md"))
        .expect("home route should exist")
        .output_path
        .clone();
    let note_html = fs::read_to_string(vault_root.join(".vulcan/site/public").join(note_route))
        .expect("note page should read");
    let home_html = fs::read_to_string(vault_root.join(".vulcan/site/public/index.html"))
        .expect("home page should read");

    assert!(note_html.contains(r"<title>Launch Page | Published Garden</title>"));
    assert!(note_html.contains("https://notes.example.com/start/"));
    assert!(note_html.contains("Custom publish summary."));
    assert!(note_html.contains("https://notes.example.com/assets/site/social.png"));
    assert!(home_html.contains("site-brand-mark"));
    assert!(vault_root
        .join(".vulcan/site/public/assets/site/social.png")
        .exists());
    assert!(vault_root
        .join(".vulcan/site/public/assets/site/logo.png")
        .exists());
}

#[test]
fn site_build_tracks_asset_copy_state_and_repairs_drifted_output_assets() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("site")).expect("site dir should exist");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(vault_root.join("Home.md"), "# Home\n\nLanding page.\n")
        .expect("home note should write");
    fs::write(vault_root.join("site/logo.png"), b"source-data").expect("logo asset should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md"]
search = false
graph = false
logo = "site/logo.png"
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let paths = VaultPaths::new(&vault_root);
    let first = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("first build should succeed");

    let output_asset = vault_root.join(".vulcan/site/public/assets/site/logo.png");
    assert_eq!(
        fs::read(&output_asset).expect("copied asset should read"),
        b"source-data"
    );
    assert!(first
        .changed_files
        .iter()
        .any(|path| path == "assets/site/logo.png"));
    assert!(
        vault_root.join(".vulcan/site-assets").exists(),
        "asset copy state directory should exist after the build"
    );

    fs::write(&output_asset, b"broken-data").expect("drifted asset should write");
    let second = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: false,
            dry_run: false,
        },
    )
    .expect("second build should succeed");

    assert!(
        second
            .changed_files
            .iter()
            .any(|path| path == "assets/site/logo.png"),
        "the drifted asset copy should be repaired: {:?}",
        second.changed_files
    );
    assert_eq!(
        fs::read(&output_asset).expect("repaired asset should read"),
        b"source-data"
    );
}

#[test]
fn site_build_repairs_drifted_note_output_when_no_notes_need_rerendering() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(vault_root.join("Home.md"), "# Home\n\nLanding page.\n")
        .expect("home note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md"]
search = false
graph = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let paths = VaultPaths::new(&vault_root);
    let first = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("first build should succeed");

    let note_output_relative = note_output_path(&first, "Home.md");
    let note_output = vault_root
        .join(".vulcan/site/public")
        .join(&note_output_relative);
    let original = fs::read_to_string(&note_output).expect("note output should read");

    fs::write(&note_output, "broken note output").expect("drifted note output should write");
    let second = build_site(
        &paths,
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: false,
            dry_run: false,
        },
    )
    .expect("second build should succeed");

    assert!(
        second
            .changed_files
            .iter()
            .any(|path| path == &note_output_relative),
        "the drifted note output should be repaired: {:?}",
        second.changed_files
    );
    assert_eq!(
        fs::read_to_string(&note_output).expect("repaired note output should read"),
        original
    );
}

#[test]
fn site_build_applies_custom_page_title_template() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should exist");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        r"# Home

Landing page.
",
    )
    .expect("home note should write");
    fs::write(
        vault_root.join("Notes/Guide.md"),
        r"# Guide

Guide page.
",
    )
    .expect("guide note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Published Garden"
page_title_template = "{site} :: {page} [{profile}]"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Notes/Guide.md"]
search = true
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");

    let home_html = fs::read_to_string(vault_root.join(".vulcan/site/public/index.html"))
        .expect("home page should read");
    let search_html = fs::read_to_string(vault_root.join(".vulcan/site/public/search/index.html"))
        .expect("search page should read");
    let note_route = report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some("Notes/Guide.md"))
        .expect("guide route should exist")
        .output_path
        .clone();
    let note_html = fs::read_to_string(vault_root.join(".vulcan/site/public").join(note_route))
        .expect("guide page should read");

    assert!(home_html.contains(r"<title>Published Garden :: Published Garden [public]</title>"));
    assert!(search_html.contains(r"<title>Published Garden :: Search [public]</title>"));
    assert!(note_html.contains(r"<title>Published Garden :: Guide [public]</title>"));
}

#[test]
fn site_build_prefixes_routes_assets_and_feeds_with_deploy_path() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        "# Home\n\nPublished home page.\n",
    )
    .expect("home note should write");
    fs::write(vault_root.join("Guide.md"), "# Guide\n\nGuide page.\n")
        .expect("guide note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Public Notes"
base_url = "https://notes.example.com"
deploy_path = "/garden"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Guide.md"]
search = true
graph = true
rss = true
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");

    assert_eq!(report.deploy_path, "/garden");
    assert!(report
        .routes
        .iter()
        .all(|route| route.url_path.starts_with("/garden/")));

    let output_root = vault_root.join(".vulcan/site/public");
    let home_html = read_site_text(&output_root, "index.html");
    let search_html = read_site_text(&output_root, "search/index.html");
    let route_manifest = read_site_json(&output_root, "assets/route-manifest.json");
    let recent_manifest = read_site_json(&output_root, "assets/recent-notes.json");
    let hover_manifest = read_site_json(&output_root, "assets/hover-previews.json");
    let rss = read_site_text(&output_root, "rss.xml");
    let sitemap = read_site_text(&output_root, "sitemap.xml");

    assert!(home_html.contains(r#"href="/garden/""#));
    assert!(home_html.contains(r#"href="/garden/assets/vulcan-site.css""#));
    assert!(home_html.contains(r#"data-live-reload-url="/garden/__vulcan_site/live-reload.json""#));
    assert!(search_html.contains(r#"data-search-asset="/garden/assets/search-index.json""#));
    assert!(search_html.contains(r#"data-graph-asset="/garden/assets/graph.json""#));
    assert_eq!(route_manifest[0]["url_path"], "/garden/notes/guide/");
    assert!(recent_manifest
        .as_array()
        .is_some_and(|entries| entries.iter().all(|entry| {
            entry["url"]
                .as_str()
                .is_some_and(|url| url.starts_with("/garden/"))
        })));
    assert!(hover_manifest["/garden/notes/home/"]["url"]
        .as_str()
        .is_some_and(|url| url == "/garden/notes/home/"));
    assert!(rss.contains("<link>https://notes.example.com/garden/</link>"));
    assert!(sitemap.contains("https://notes.example.com/garden/notes/home/"));
    assert!(sitemap.contains("https://notes.example.com/garden/rss.xml"));
}

#[test]
fn site_build_regression_covers_route_manifest_and_note_page_html() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Guide.md"),
        r#"---
site:
  profiles:
    public:
      description: Guide summary.
      hide_modules: ["toc"]
---

# Guide

Body text.
"#,
    )
    .expect("guide note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Public Notes"
output_dir = ".vulcan/site/public"
include_paths = ["Guide.md"]
search = false
graph = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");
    let output_root = vault_root.join(".vulcan/site/public");
    let route_manifest = read_site_json(&output_root, "assets/route-manifest.json");
    let note_route = report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some("Guide.md"))
        .expect("guide route should exist")
        .output_path
        .clone();
    let note_html = read_site_text(&output_root, &note_route);

    assert_eq!(
        route_manifest,
        serde_json::json!([
            {
                "kind": "note",
                "source_path": "Guide.md",
                "title": "Guide",
                "slug": "Guide",
                "url_path": "/notes/guide/",
                "output_path": "notes/guide/index.html"
            }
        ])
    );
    assert!(note_html.contains(r#"data-site-profile="public""#));
    assert!(note_html.contains(r#"data-default-palette="system""#));
    assert!(note_html.contains(r#"data-reader-mode-enabled="true""#));
    assert!(note_html.contains(r#"data-left-rail-enabled="true""#));
    assert!(note_html.contains(r#"id="site-left-rail""#));
    assert!(note_html.contains(r#"id="site-right-rail""#));
    assert!(note_html.contains(r#"class="site-mobile-dock""#));
    assert!(!note_html.contains(r#"class="site-toolbar-bar""#));
    assert!(note_html.contains(r#"data-theme-mode="system""#));
    assert!(note_html.contains(r"data-reader-mode-toggle"));
    assert!(note_html.contains(r#"class="site-explorer-tree""#));
    assert!(!note_html.contains(r#"class="site-module-toolbar""#));
    assert!(!note_html.contains(r#"data-site-module="toc""#));
    assert!(note_html.contains(r"Guide.md"));
    assert!(note_html.contains(r#"<h1 id="guide">Guide</h1>"#));
}

#[test]
fn site_build_default_shell_accessibility_smoke() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Notes")).expect("notes dir should exist");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(vault_root.join("Home.md"), "# Home\n\nLanding page.\n")
        .expect("home note should write");
    fs::write(
        vault_root.join("Notes/Guide.md"),
        "# Guide\n\nLinked page.\n",
    )
    .expect("guide note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Published Garden"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Notes/Guide.md"]
search = true
graph = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");
    let output_root = vault_root.join(".vulcan/site/public");
    let home_html = read_site_text(&output_root, "index.html");
    let search_html = read_site_text(&output_root, "search/index.html");
    let folders_html = read_site_text(&output_root, "folders/index.html");
    let recent_html = read_site_text(&output_root, "recent/index.html");
    let guide_route = report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some("Notes/Guide.md"))
        .expect("guide route should exist")
        .output_path
        .clone();
    let guide_html = read_site_text(&output_root, &guide_route);

    for html in [&home_html, &search_html, &guide_html] {
        assert!(html.contains("href=\"#main-content\""));
        assert!(html.contains(r#"id="main-content""#));
        assert!(html.contains(r#"aria-label="Primary""#));
        assert!(html.contains(r#"aria-label="Page context""#));
        assert!(html.contains(r#"class="site-mobile-dock""#));
    }
    assert_eq!(count_occurrences(&home_html, "<h1"), 1);
    assert_eq!(count_occurrences(&search_html, "<h1"), 1);
    assert_eq!(count_occurrences(&guide_html, "<h1"), 1);
    assert!(guide_html.contains(r#"aria-label="Breadcrumbs""#));
    assert!(home_html.contains(r"data-site-explorer-filter"));
    assert!(home_html.contains(r#"data-site-explorer-filter-text="notes""#));
    assert!(search_html.contains(
            r#"<label class="site-visually-hidden" for="site-search-dialog-input">Search published notes</label>"#
        ));
    assert!(search_html.contains(r"data-site-search-open"));
    assert!(search_html.contains(r#"type="search""#));
    assert!(search_html.contains(r#"aria-describedby="site-search-dialog-hint""#));
    assert!(search_html.contains(r#"aria-keyshortcuts="/""#));
    assert!(search_html.contains(r#"aria-live="polite""#));
    assert!(folders_html.contains(r#"class="site-listing-hero""#));
    assert!(folders_html.contains("Folder explorer"));
    assert!(folders_html.contains(r#"class="site-listing-stat""#));
    assert!(recent_html.contains("Recently updated"));
}

#[test]
fn site_build_emits_math_and_mermaid_runtime_contract() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        concat!(
            "# Home\n\n",
            "Inline math: $x + y$.\n\n",
            "$$\n",
            "x^2 + y^2 = z^2\n",
            "$$\n\n",
            "```mermaid\n",
            "flowchart TD\n",
            "  A --> B\n",
            "```\n",
        ),
    )
    .expect("home note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Published Garden"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md"]
search = false
graph = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");

    let output_root = vault_root.join(".vulcan/site/public");
    let home_html = read_site_text(&output_root, "index.html");
    let shell_js = read_site_text(&output_root, "assets/vulcan-site.js");

    assert!(home_html.contains(r#"data-site-math="inline""#));
    assert!(home_html.contains(r#"data-site-math="display""#));
    assert!(home_html.contains(r#"data-site-mermaid-source="true""#));
    assert!(home_html.contains(r#"class="language-mermaid""#));

    assert!(shell_js.contains("vulcan-site:math"));
    assert!(shell_js.contains("vulcan-site:mermaid"));
    assert!(shell_js.contains("window.katex"));
    assert!(shell_js.contains("window.mermaid"));
    assert!(shell_js.contains("data-site-mermaid"));
}

#[test]
fn site_build_uses_shared_renderer_for_dataview_outputs() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("dataview", &vault_root);
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Dataview Demo"
output_dir = ".vulcan/site/public"
include_paths = ["Dashboard.md"]
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");

    let dashboard_route = report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some("Dashboard.md"))
        .expect("dashboard route should exist")
        .output_path
        .clone();
    let dashboard_html =
        fs::read_to_string(vault_root.join(".vulcan/site/public").join(dashboard_route))
            .expect("dashboard page should read");
    assert!(dashboard_html.contains(r#"class="dql-table""#));
    assert!(dashboard_html.contains("DataviewJS disabled"));
}

#[test]
fn site_build_emits_folder_note_navigation_manifest_and_profile_shell_state() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Guides")).expect("guides dir should exist");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(vault_root.join("Home.md"), "# Home\n\nLanding page.\n")
        .expect("home note should write");
    fs::write(
        vault_root.join("Guides/index.md"),
        "# Guides\n\nFolder landing page.\n",
    )
    .expect("folder note should write");
    fs::write(
        vault_root.join("Guides/Intro.md"),
        "# Intro\n\nGuide body.\n",
    )
    .expect("intro note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Docs"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Guides/index.md", "Guides/Intro.md"]
search = false
graph = false
backlinks = false

[site.profiles.public.shell]
reader_mode = false
default_palette = "dark"

[site.profiles.public.navigation]
folder_click = "collapse"
default_folder_state = "open"
use_saved_state = false

[site.profiles.public.modules]
graph = false
backlinks = false
outgoing_links = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");
    let output_root = vault_root.join(".vulcan/site/public");
    let intro_html = read_site_text(&output_root, &note_output_path(&report, "Guides/Intro.md"));
    let navigation_tree = read_site_json(&output_root, "assets/navigation-tree.json");
    let folder_note_route = report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some("Guides/index.md"))
        .expect("folder note route should exist");
    let guides_node = navigation_tree
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["title"] == "Guides"))
        .expect("guides folder should exist in navigation manifest");

    assert_eq!(guides_node["kind"], "folder");
    assert_eq!(guides_node["source_path"], "Guides/index.md");
    assert_eq!(guides_node["url"], folder_note_route.url_path);
    assert!(guides_node["children"]
        .as_array()
        .is_some_and(|children| children.iter().any(|child| child["title"] == "Intro")));

    assert!(intro_html.contains(r#"data-default-palette="dark""#));
    assert!(intro_html.contains(r#"data-reader-mode-enabled="false""#));
    assert!(intro_html.contains(r#"data-site-folder-click="collapse""#));
    assert!(intro_html.contains(r#"data-site-folder-state="open""#));
    assert!(intro_html.contains(r#"data-site-saved-state="false""#));
    assert!(intro_html.contains(r#"data-site-module="toc""#));
    assert!(!intro_html.contains(r#"data-site-module="graph""#));
    assert!(!intro_html.contains(r#"data-site-module="backlinks""#));
}

#[test]
fn site_build_rewrites_markdown_links_and_supports_same_name_folder_notes() {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    fs::create_dir_all(vault_root.join("Projects")).expect("projects dir should exist");
    fs::create_dir_all(vault_root.join("Guides")).expect("guides dir should exist");
    fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
    fs::write(
        vault_root.join("Home.md"),
        concat!(
            "# Home\n\n",
            "[Alpha](Projects/Alpha) and [Guides](Guides)\n\n",
            "- [ ] Publish polish pass\n",
        ),
    )
    .expect("home note should write");
    fs::write(
        vault_root.join("Projects/Alpha.md"),
        "# Alpha\n\nProject note.\n",
    )
    .expect("alpha note should write");
    fs::write(
        vault_root.join("Guides/Guides.md"),
        "# Guides\n\nFolder note.\n",
    )
    .expect("folder note should write");
    fs::write(
        vault_root.join("Guides/Intro.md"),
        "# Intro\n\nGuide body.\n",
    )
    .expect("guide note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Docs"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md", "Guides/Guides.md", "Guides/Intro.md"]
search = false
graph = false
backlinks = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");

    let output_root = vault_root.join(".vulcan/site/public");
    let home_html = read_site_text(&output_root, "index.html");
    let navigation_tree = read_site_json(&output_root, "assets/navigation-tree.json");
    let folder_note_route = report
        .routes
        .iter()
        .find(|route| route.source_path.as_deref() == Some("Guides/Guides.md"))
        .expect("same-name folder note route should exist");
    let guides_node = navigation_tree
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["title"] == "Guides"))
        .expect("guides folder should exist in navigation manifest");

    assert!(home_html.contains(r#"href="/notes/projects/alpha/""#));
    assert!(home_html.contains(r#"href="/notes/guides/""#));
    assert!(home_html.contains(r#"type="checkbox""#));
    assert!(home_html.contains(r#"data-site-folder-click="collapse""#));
    assert!(home_html.contains(r#"class="site-explorer-folder-open""#));
    assert_eq!(folder_note_route.url_path, "/notes/guides/");
    assert_eq!(guides_node["source_path"], "Guides/Guides.md");
    assert_eq!(guides_node["url"], "/notes/guides/");
    assert!(guides_node["children"]
        .as_array()
        .is_some_and(|children| children.iter().any(|child| child["title"] == "Intro")));
}

#[test]
fn search_text_from_markdown_strips_frontmatter_and_markdown_chrome() {
    let text = search_text_from_markdown(concat!(
        "---\n",
        "title: Demo\n",
        "---\n\n",
        "# Heading\n\n",
        "Text with [[Wiki Links]] and [aliases](Target.md).\n\n",
        "- [ ] Task item\n",
        "```rust\n",
        "fn demo() {}\n",
        "```\n",
    ));

    assert_eq!(
        text,
        "Heading Text with Wiki Links and aliases Target.md. Task item"
    );
}

#[test]
fn build_search_index_uses_source_search_text_and_aliases() {
    let rendered_notes = vec![RenderedNote {
        source_path: "Guides/Intro.md".to_string(),
        title: "Intro".to_string(),
        excerpt: "Short summary.".to_string(),
        description: "Short summary.".to_string(),
        canonical_url: None,
        summary_image: None,
        route: SiteRoute {
            kind: "note".to_string(),
            source_path: Some("Guides/Intro.md".to_string()),
            title: "Intro".to_string(),
            slug: "guides/intro".to_string(),
            url_path: "/notes/guides/intro/".to_string(),
            output_path: "notes/guides/intro/index.html".to_string(),
        },
        html: "<p>Rendered HTML should not control search text.</p>".to_string(),
        headings: Vec::new(),
        tags: vec!["docs".to_string()],
        aliases: vec!["Getting Started".to_string()],
        outgoing_links: Vec::new(),
        backlinks: Vec::new(),
        hidden_modules: Vec::new(),
        breadcrumbs: Vec::new(),
        asset_paths: Vec::new(),
        embeds: Vec::new(),
        diagnostics: Vec::new(),
        file_mtime: 0,
    }];
    let search_text_by_path = HashMap::from([(
        "Guides/Intro.md".to_string(),
        "markdown source terms and getting started body".to_string(),
    )]);

    let index = build_search_index(&rendered_notes, &search_text_by_path);
    let documents = index["documents"]
        .as_array()
        .expect("documents should serialize");
    let terms = index["terms"].as_object().expect("terms should serialize");

    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0]["preview"], "Short summary.");
    assert!(terms.contains_key("markdown"));
    assert!(terms.contains_key("getting"));
    assert!(terms.contains_key("started"));
}

#[test]
fn site_build_renders_hardening_fixture_surfaces_for_bases_tasks_tasknotes_kanban_and_periodic_notes(
) {
    let temp_dir = TempDir::new().expect("temp dir should exist");
    let vault_root = temp_dir.path().join("vault");
    copy_fixture_vault("hardening", &vault_root);
    fs::write(
        vault_root.join("Showcase.md"),
        concat!(
            "# Showcase\n\n",
            "![[Reports/release.base#Release Table]]\n\n",
            "```tasks\n",
            "path includes TaskNotes/Tasks\n",
            "not done\n",
            "sort by due\n",
            "```\n",
        ),
    )
    .expect("showcase note should write");
    fs::write(
        vault_root.join(".vulcan/config.toml"),
        r#"[site.profiles.public]
title = "Hardening Site"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Dashboard.md", "Showcase.md"]
include_folders = ["Boards/**", "Journal/**", "People/**", "Projects/**", "TaskNotes/**"]
exclude_paths = ["Broken.md"]
search = false
graph = false
rss = false
"#,
    )
    .expect("config should write");
    scan_fixture(&vault_root);

    let report = build_site(
        &VaultPaths::new(&vault_root),
        &SiteBuildRequest {
            profile: Some("public".to_string()),
            output_dir: None,
            clean: true,
            dry_run: false,
        },
    )
    .expect("site build should succeed");
    assert!(
        report.diagnostics.is_empty(),
        "hardening fixture site build should stay clean, got: {:?}",
        report.diagnostics
    );

    let output_root = vault_root.join(".vulcan/site/public");
    let route_html = |source_path: &str| {
        let relative = report
            .routes
            .iter()
            .find(|route| route.source_path.as_deref() == Some(source_path))
            .unwrap_or_else(|| panic!("route should exist for {source_path}"))
            .output_path
            .clone();
        read_site_text(&output_root, &relative)
    };

    let dashboard_html = route_html("Dashboard.md");
    let showcase_html = route_html("Showcase.md");
    let board_html = route_html("Boards/Roadmap.md");
    let daily_html = route_html("Journal/Daily/2026-04-04.md");
    let task_html = route_html("TaskNotes/Tasks/Write Docs.md");

    assert!(dashboard_html.contains("Release Dashboard"));
    assert!(dashboard_html.contains("in-progress"));
    assert!(dashboard_html.contains("2026-04-10"));
    assert!(dashboard_html.contains("2026-04-04"));

    assert!(showcase_html.contains("Release Table"));
    assert!(showcase_html.contains(r#"class="bases-table""#));
    assert!(showcase_html.contains(r#"class="tasks-query-list""#));
    assert!(showcase_html.contains("Write docs"));
    let showcase_tasks = showcase_html
        .split(r#"class="tasks-query-list""#)
        .nth(1)
        .and_then(|section| section.split("</ul>").next())
        .expect("tasks query section should render");
    assert!(!showcase_tasks.contains("Prep outline"));

    assert!(board_html.contains("Release Board"));
    assert!(board_html.contains("Backlog"));
    assert!(board_html.contains("Draft release email"));
    assert!(board_html.contains("Write Docs"));

    assert!(daily_html.contains("2026-04-04"));
    assert!(daily_html.contains("Review"));
    assert!(daily_html.contains("Finish"));
    assert!(daily_html.contains("Write Docs"));

    assert!(task_html.contains("Write docs"));
    assert!(task_html.contains("Write the docs body."));
}
