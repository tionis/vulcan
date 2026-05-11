#![allow(
    clippy::format_collect,
    clippy::struct_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

use crate::export::{
    content_transform_rules_have_effective_transforms, load_export_links_for_notes,
    prepare_export_data, ExportLinkRecord,
};
use crate::AppError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;
use vulcan_core::config::{
    load_vault_config, ContentTransformRuleConfig, SiteAssetPolicyConfig,
    SiteAssetPolicyModeConfig, SiteDataviewJsPolicyConfig, SiteExplorerFolderStateConfig,
    SiteFolderClickBehaviorConfig, SiteLinkPolicyConfig, SitePaletteModeConfig, SiteProfileConfig,
    SiteRawHtmlPolicyConfig,
};
use vulcan_core::graph::resolve_note_reference;
use vulcan_core::html::{
    HtmlDataviewJsPolicy, HtmlLinkTargets, HtmlRawHtmlPolicy, HtmlRenderDiagnostic,
    HtmlRenderHeading, HtmlRenderOptions, VaultHtmlRenderer,
};
use vulcan_core::properties::NoteRecord;
use vulcan_core::query::{execute_query_report, QueryAst, QueryReport};
use vulcan_core::{ensure_vulcan_dir, export_graph, parse_document, VaultPaths};

mod assets;
mod types;

use assets::{DEFAULT_THEME_CSS, DEFAULT_THEME_JS};

pub use types::*;

const DEFAULT_PAGE_TITLE_TEMPLATE: &str = "{page} | {site}";
const SITE_BUILD_STATE_VERSION: u32 = 1;
const SITE_ASSET_COPY_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone)]
struct ResolvedSiteProfile {
    name: String,
    title: String,
    page_title_template: String,
    output_dir: PathBuf,
    base_url: Option<String>,
    deploy_path: String,
    home: Option<String>,
    language: String,
    theme: String,
    search: bool,
    graph: bool,
    backlinks: bool,
    rss: bool,
    shell: ResolvedSiteShell,
    navigation: ResolvedSiteNavigation,
    modules: ResolvedSiteModules,
    favicon: Option<PathBuf>,
    logo: Option<PathBuf>,
    extra_css: Vec<PathBuf>,
    extra_js: Vec<PathBuf>,
    include_query: Option<String>,
    include_query_json: Option<String>,
    include_paths: Vec<String>,
    include_folders: Vec<String>,
    exclude_paths: Vec<String>,
    exclude_folders: Vec<String>,
    exclude_tags: Vec<String>,
    link_policy: SiteLinkPolicyConfig,
    asset_policy: SiteAssetPolicyConfig,
    dataview_js: SiteDataviewJsPolicyConfig,
    raw_html: SiteRawHtmlPolicyConfig,
    theme_overrides: ResolvedSiteTheme,
    content_transform_rules: Option<Vec<ContentTransformRuleConfig>>,
    implicit: bool,
}

#[derive(Debug, Clone)]
struct SitePlanNote {
    note: NoteRecord,
    published_source: Option<String>,
}

#[derive(Debug, Clone)]
struct SitePlan {
    profile: ResolvedSiteProfile,
    notes: Vec<SitePlanNote>,
    links: Vec<ExportLinkRecord>,
    routes: Vec<SiteRoute>,
    diagnostics: Vec<SiteDiagnostic>,
    all_note_signatures: BTreeMap<String, SiteInputSignature>,
    config_signature: String,
    transforms_active: bool,
}

#[derive(Debug, Clone, Default)]
struct ResolvedSiteTheme {
    css_assets: Vec<PathBuf>,
    js_assets: Vec<PathBuf>,
    head: Option<String>,
    header: Option<String>,
    nav: Option<String>,
    toolbar: Option<String>,
    left_rail: Option<String>,
    right_rail: Option<String>,
    footer: Option<String>,
    note_before: Option<String>,
    note_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ResolvedSiteShell {
    reader_mode: bool,
    default_palette: SitePaletteModeConfig,
    left_rail: bool,
    right_rail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ResolvedSiteNavigation {
    explorer: bool,
    folder_click: SiteFolderClickBehaviorConfig,
    default_folder_state: SiteExplorerFolderStateConfig,
    use_saved_state: bool,
    show_home: bool,
    show_recent: bool,
    show_folders: bool,
    show_tags: bool,
    show_graph: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ResolvedSiteModules {
    toc: bool,
    graph: bool,
    backlinks: bool,
    outgoing_links: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SiteNavigationNode {
    kind: String,
    title: String,
    id: String,
    url: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<SiteNavigationNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    folder_path: Option<String>,
}

#[derive(Debug, Clone)]
struct SiteShellModule {
    id: &'static str,
    title: &'static str,
    body_html: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SiteInputSignature {
    file_mtime: i64,
    file_size: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SiteFileDependency {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<SiteInputSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SiteNoteDependencies {
    link_state_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    note_embeds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    base_embeds: Vec<SiteFileDependency>,
    has_vault_queries: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SiteBuildStateNote {
    source_path: String,
    signature: SiteInputSignature,
    rendered: RenderedNote,
    #[serde(default)]
    search_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page_output_signature: Option<SiteInputSignature>,
    dependencies: SiteNoteDependencies,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SiteBuildState {
    version: u32,
    config_signature: String,
    all_note_signatures: BTreeMap<String, SiteInputSignature>,
    notes: Vec<SiteBuildStateNote>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SiteCopiedAssetState {
    source_path: String,
    source_signature: SiteInputSignature,
    output_signature: SiteInputSignature,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SiteAssetCopyState {
    version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    assets: Vec<SiteCopiedAssetState>,
}

#[derive(Debug, Clone)]
struct SiteSelectedNotes {
    selected: Vec<NoteRecord>,
    all_note_signatures: BTreeMap<String, SiteInputSignature>,
}

struct SiteRenderShared<'a> {
    route_map: HashMap<String, SiteRoute>,
    published_paths: HashSet<String>,
    asset_hrefs: HashMap<String, String>,
    link_targets: HtmlLinkTargets,
    links_by_source: HashMap<&'a str, Vec<&'a ExportLinkRecord>>,
    backlinks: HashMap<String, Vec<String>>,
}

struct SiteRenderOutcome {
    rendered_notes: Vec<RenderedNote>,
    search_text_by_path: HashMap<String, String>,
    next_state: Option<SiteBuildState>,
    rendered_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SiteAssetCopyWorkItem {
    source_path: PathBuf,
    source_key: String,
    relative_output_path: String,
    destination: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SiteAssetCopyResult {
    changed: bool,
    state: SiteCopiedAssetState,
}

#[derive(Debug, Clone, Serialize)]
struct SearchIndexDocument {
    id: usize,
    title: String,
    url: String,
    excerpt: String,
    preview: String,
    tags: Vec<String>,
    length: usize,
}

#[derive(Debug, Clone, Serialize)]
struct SearchIndexPosting {
    id: usize,
    tf: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RecentNoteManifestEntry {
    source_path: String,
    title: String,
    url: String,
    excerpt: String,
    file_mtime: i64,
    tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RelatedNoteManifestEntry {
    source_path: String,
    title: String,
    url: String,
    excerpt: String,
    shared_tags: Vec<String>,
}

pub fn build_site_profiles_report(
    paths: &VaultPaths,
) -> Result<Vec<SiteProfileListEntry>, AppError> {
    let profile_names = available_site_profile_names(paths);
    profile_names
        .into_iter()
        .map(|name| {
            let profile = resolve_site_profile(paths, Some(name.as_str()), None)?;
            let note_count = select_site_notes(paths, &profile)?.selected.len();
            Ok(SiteProfileListEntry {
                name: profile.name.clone(),
                title: profile.title.clone(),
                output_dir: display_path(&profile.output_dir),
                deploy_path: profile.deploy_path.clone(),
                note_count,
                search: profile.search,
                graph: profile.graph,
                backlinks: profile.backlinks,
                rss: profile.rss,
                theme: profile.theme.clone(),
                implicit: profile.implicit,
            })
        })
        .collect()
}

pub fn build_site_doctor_report(
    paths: &VaultPaths,
    profile_name: Option<&str>,
) -> Result<SiteDoctorReport, AppError> {
    let plan = plan_site(paths, profile_name, None)?;
    Ok(SiteDoctorReport {
        profile: plan.profile.name,
        note_count: plan.notes.len(),
        diagnostics: plan.diagnostics,
        routes: plan.routes,
    })
}

pub fn build_site(
    paths: &VaultPaths,
    request: &SiteBuildRequest,
) -> Result<SiteBuildReport, AppError> {
    build_site_with_progress(paths, request, |_| {})
}

pub fn build_site_with_progress<F>(
    paths: &VaultPaths,
    request: &SiteBuildRequest,
    mut progress: F,
) -> Result<SiteBuildReport, AppError>
where
    F: FnMut(&SiteBuildProgress),
{
    report_site_build_progress(&mut progress, SiteBuildPhase::Planning, 0, 0, None);
    let plan = plan_site(
        paths,
        request.profile.as_deref(),
        request.output_dir.as_deref(),
    )?;
    let cached_state = if plan.transforms_active {
        None
    } else {
        load_site_build_state(paths, &plan.profile)?
    };
    let cached_asset_state = load_site_asset_copy_state(paths, &plan.profile)?;
    if request.clean && !request.dry_run && plan.profile.output_dir.exists() {
        fs::remove_dir_all(&plan.profile.output_dir).map_err(AppError::operation)?;
    }
    let output_dir = plan.profile.output_dir.clone();
    let SiteRenderOutcome {
        mut rendered_notes,
        search_text_by_path,
        mut next_state,
        rendered_count,
    } = render_site_notes(paths, &plan, cached_state.as_ref(), &mut progress)?;
    rendered_notes.sort_by(|left, right| left.route.url_path.cmp(&right.route.url_path));

    let route_urls = rendered_notes
        .iter()
        .map(|note| (note.source_path.clone(), note.route.url_path.clone()))
        .collect::<HashMap<_, _>>();
    let cached_assets_by_source = cached_asset_state
        .as_ref()
        .map(site_asset_copy_state_by_source);
    let asset_work_items =
        collect_site_asset_copy_work_items(paths, &plan, &rendered_notes, &output_dir)?;
    let mut files = BTreeSet::<String>::new();
    let mut changed_files = BTreeSet::<String>::new();
    let asset_count = asset_work_items.len();
    let mut next_asset_state = Vec::with_capacity(asset_work_items.len());
    report_site_build_progress(
        &mut progress,
        SiteBuildPhase::CopyingAssets,
        0,
        asset_work_items.len(),
        None,
    );

    if !request.dry_run {
        fs::create_dir_all(&output_dir).map_err(AppError::operation)?;
    }

    let context = RenderContext {
        profile: plan.profile.name.clone(),
        site_title: plan.profile.title.clone(),
        language: plan.profile.language.clone(),
        theme: plan.profile.theme.clone(),
        base_url: plan.profile.base_url.clone(),
        deploy_path: plan.profile.deploy_path.clone(),
    };

    let tag_index = build_tag_index(&rendered_notes);
    let folder_index = build_folder_index(&rendered_notes);
    let home_note = resolve_home_note(&plan.profile, &rendered_notes);
    let navigation_tree = build_navigation_tree(&context.deploy_path, &rendered_notes);
    let next_state_note_indices = next_state.as_ref().map(|state| {
        state
            .notes
            .iter()
            .enumerate()
            .map(|(index, note)| (note.source_path.clone(), index))
            .collect::<HashMap<_, _>>()
    });
    let can_reuse_note_page_outputs = rendered_count == 0 && !request.dry_run;

    for (index, note) in rendered_notes.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .and_then(|value| rendered_notes.get(value));
        let next = rendered_notes.get(index + 1);
        let state_note_index = next_state_note_indices
            .as_ref()
            .and_then(|indices| indices.get(&note.source_path))
            .copied();
        let path = output_dir.join(&note.route.output_path);
        let cached_output_signature = state_note_index.and_then(|note_index| {
            next_state
                .as_ref()
                .and_then(|state| state.notes.get(note_index))
                .and_then(|state_note| state_note.page_output_signature.clone())
        });
        if can_reuse_note_page_outputs
            && match cached_output_signature.as_ref() {
                Some(cached_signature) => site_path_signature_matches(&path, cached_signature)?,
                None => false,
            }
        {
            files.insert(note.route.output_path.clone());
            continue;
        }

        let html = render_note_document(
            &context,
            note,
            previous,
            next,
            &plan.profile,
            &navigation_tree,
            &route_urls,
            &tag_index,
            &folder_index,
            home_note,
        );
        let output_signature = if request.dry_run {
            None
        } else {
            if write_output_file(&path, &html)? {
                changed_files.insert(note.route.output_path.clone());
            }
            Some(site_input_signature_for_path(&path)?.ok_or_else(|| {
                AppError::operation(format!(
                    "note page disappeared before it could be tracked: {}",
                    path.display()
                ))
            })?)
        };
        if let (Some(note_index), Some(state)) = (state_note_index, next_state.as_mut()) {
            state.notes[note_index].page_output_signature = output_signature;
        }
        files.insert(note.route.output_path.clone());
    }

    for asset in &asset_work_items {
        if !request.dry_run {
            let copied = copy_file_from_vault_with_cache(
                paths,
                &asset.source_path,
                &asset.destination,
                cached_assets_by_source
                    .as_ref()
                    .and_then(|assets| assets.get(asset.source_key.as_str()).copied()),
            )?;
            if copied.changed {
                changed_files.insert(asset.relative_output_path.clone());
            }
            next_asset_state.push(copied.state);
        }
        files.insert(asset.relative_output_path.clone());
    }

    let css_path = output_dir.join("assets/vulcan-site.css");
    let js_path = output_dir.join("assets/vulcan-site.js");
    if !request.dry_run {
        if write_output_file(&css_path, DEFAULT_THEME_CSS)? {
            changed_files.insert("assets/vulcan-site.css".to_string());
        }
        if write_output_file(&js_path, DEFAULT_THEME_JS)? {
            changed_files.insert("assets/vulcan-site.js".to_string());
        }
    }
    files.insert("assets/vulcan-site.css".to_string());
    files.insert("assets/vulcan-site.js".to_string());

    let manifest = serde_json::to_string_pretty(&plan.routes).map_err(AppError::operation)?;
    let manifest_path = output_dir.join("assets/route-manifest.json");
    if !request.dry_run && write_output_file(&manifest_path, &manifest)? {
        changed_files.insert("assets/route-manifest.json".to_string());
    }
    files.insert("assets/route-manifest.json".to_string());

    let navigation_manifest =
        serde_json::to_string_pretty(&navigation_tree).map_err(AppError::operation)?;
    let navigation_manifest_path = output_dir.join("assets/navigation-tree.json");
    if !request.dry_run && write_output_file(&navigation_manifest_path, &navigation_manifest)? {
        changed_files.insert("assets/navigation-tree.json".to_string());
    }
    files.insert("assets/navigation-tree.json".to_string());

    let hover_manifest = build_hover_manifest(&rendered_notes);
    let hover_manifest_json =
        serde_json::to_string_pretty(&hover_manifest).map_err(AppError::operation)?;
    let hover_path = output_dir.join("assets/hover-previews.json");
    if !request.dry_run && write_output_file(&hover_path, &hover_manifest_json)? {
        changed_files.insert("assets/hover-previews.json".to_string());
    }
    files.insert("assets/hover-previews.json".to_string());

    let recent_manifest = build_recent_manifest(&rendered_notes);
    let recent_manifest_json =
        serde_json::to_string_pretty(&recent_manifest).map_err(AppError::operation)?;
    let recent_manifest_path = output_dir.join("assets/recent-notes.json");
    if !request.dry_run && write_output_file(&recent_manifest_path, &recent_manifest_json)? {
        changed_files.insert("assets/recent-notes.json".to_string());
    }
    files.insert("assets/recent-notes.json".to_string());

    let related_manifest = build_related_manifest(&rendered_notes, &tag_index);
    let related_manifest_json =
        serde_json::to_string_pretty(&related_manifest).map_err(AppError::operation)?;
    let related_manifest_path = output_dir.join("assets/related-notes.json");
    if !request.dry_run && write_output_file(&related_manifest_path, &related_manifest_json)? {
        changed_files.insert("assets/related-notes.json".to_string());
    }
    files.insert("assets/related-notes.json".to_string());

    if plan.profile.search {
        report_site_build_progress(
            &mut progress,
            SiteBuildPhase::WritingSearchIndex,
            0,
            0,
            None,
        );
        let search_index = build_search_index(&rendered_notes, &search_text_by_path);
        let search_json =
            serde_json::to_string_pretty(&search_index).map_err(AppError::operation)?;
        let search_path = output_dir.join("assets/search-index.json");
        if !request.dry_run {
            if write_output_file(&search_path, &search_json)? {
                changed_files.insert("assets/search-index.json".to_string());
            }
            if write_output_file(
                &output_dir.join("search/index.html"),
                &render_generic_page(
                    &context,
                    "Search",
                    "Find published notes with keyboard-first search.",
                    concat!(
                        "<section class=\"site-search-card\" aria-labelledby=\"site-search-title\">",
                        "<h2 id=\"site-search-title\">Search published notes</h2>",
                        "<p class=\"site-meta\">Use the left rail search button or press / anywhere in the site. This page keeps the same search UI available as a focused mobile sheet.</p>",
                        "<button class=\"site-search-launch\" type=\"button\" data-site-search-open>Open search</button>",
                        "</section>"
                    ),
                    &navigation_tree,
                    &plan.profile,
                    true,
                    &prefixed_site_path(&context.deploy_path, "/search/"),
                ),
            )? {
                changed_files.insert("search/index.html".to_string());
            }
        }
        files.insert("assets/search-index.json".to_string());
        files.insert("search/index.html".to_string());
    }

    if plan.profile.graph {
        report_site_build_progress(&mut progress, SiteBuildPhase::WritingGraph, 0, 0, None);
        let graph_json = build_graph_asset(paths, &rendered_notes)?;
        let graph_path = output_dir.join("assets/graph.json");
        if !request.dry_run {
            if write_output_file(&graph_path, &graph_json)? {
                changed_files.insert("assets/graph.json".to_string());
            }
            let graph_card = concat!(
                "<section class=\"site-graph-card\">",
                "<h2>Published graph</h2>",
                "<p class=\"site-meta\">Explore the published subset as a client-side graph. The graph page shows the highest-degree published notes first, while note pages render a local neighborhood in the right rail.</p>",
                "<div class=\"site-graph-stage is-global\" data-site-graph-canvas=\"global\">",
                "<p class=\"site-graph-empty\">Loading published graph…</p>",
                "</div>",
                "</section>"
            );
            if write_output_file(
                &output_dir.join("graph/index.html"),
                &render_generic_page(
                    &context,
                    "Graph",
                    "Graph export",
                    graph_card,
                    &navigation_tree,
                    &plan.profile,
                    false,
                    &prefixed_site_path(&context.deploy_path, "/graph/"),
                ),
            )? {
                changed_files.insert("graph/index.html".to_string());
            }
        }
        files.insert("assets/graph.json".to_string());
        files.insert("graph/index.html".to_string());
    }

    if plan.profile.rss && plan.profile.base_url.is_some() {
        let rss = build_rss_document(&context, &rendered_notes);
        if !request.dry_run && write_output_file(&output_dir.join("rss.xml"), &rss)? {
            changed_files.insert("rss.xml".to_string());
        }
        files.insert("rss.xml".to_string());
    }

    report_site_build_progress(&mut progress, SiteBuildPhase::WritingPages, 0, 0, None);
    let folder_pages =
        render_folder_pages(&context, &folder_index, &navigation_tree, &plan.profile);
    for (relative_path, body) in folder_pages {
        if !request.dry_run && write_output_file(&output_dir.join(&relative_path), &body)? {
            changed_files.insert(relative_path.clone());
        }
        files.insert(relative_path);
    }

    let tag_pages = render_tag_pages(&context, &tag_index, &navigation_tree, &plan.profile);
    for (relative_path, body) in tag_pages {
        if !request.dry_run && write_output_file(&output_dir.join(&relative_path), &body)? {
            changed_files.insert(relative_path.clone());
        }
        files.insert(relative_path);
    }

    let recent_html =
        render_recent_page(&context, &rendered_notes, &navigation_tree, &plan.profile);
    if !request.dry_run && write_output_file(&output_dir.join("recent/index.html"), &recent_html)? {
        changed_files.insert("recent/index.html".to_string());
    }
    files.insert("recent/index.html".to_string());

    if let Some(home) = home_note.as_ref() {
        let home_html = render_home_page(
            &context,
            home,
            &navigation_tree,
            &plan.profile,
            &tag_index,
            &folder_index,
        );
        if !request.dry_run && write_output_file(&output_dir.join("index.html"), &home_html)? {
            changed_files.insert("index.html".to_string());
        }
    } else {
        let body = render_listing_cards(&rendered_notes);
        if !request.dry_run
            && write_output_file(
                &output_dir.join("index.html"),
                &render_generic_page(
                    &context,
                    &plan.profile.title,
                    "Published notes",
                    &body,
                    &navigation_tree,
                    &plan.profile,
                    false,
                    &site_root_href(&context.deploy_path),
                ),
            )?
        {
            changed_files.insert("index.html".to_string());
        }
    }
    files.insert("index.html".to_string());

    if let Some(base_url) = context.base_url.as_deref() {
        let sitemap = build_sitemap(base_url, &context.deploy_path, &files, &rendered_notes);
        if !request.dry_run && write_output_file(&output_dir.join("sitemap.xml"), &sitemap)? {
            changed_files.insert("sitemap.xml".to_string());
        }
        files.insert("sitemap.xml".to_string());
    }

    let deleted_files = if request.dry_run {
        Vec::new()
    } else {
        remove_stale_output_files(&output_dir, &files)?
    };
    report_site_build_progress(&mut progress, SiteBuildPhase::Finalizing, 0, 0, None);
    changed_files.extend(deleted_files.iter().cloned());
    if !request.dry_run {
        save_site_asset_copy_state(
            paths,
            &plan.profile,
            &SiteAssetCopyState {
                version: SITE_ASSET_COPY_STATE_VERSION,
                assets: next_asset_state,
            },
        )?;
        if let Some(state) = next_state.as_ref() {
            save_site_build_state(paths, &plan.profile, state)?;
        }
    }
    let file_list = files.into_iter().collect::<Vec<_>>();
    Ok(SiteBuildReport {
        profile: plan.profile.name,
        output_dir: display_path(&output_dir),
        deploy_path: plan.profile.deploy_path,
        dry_run: request.dry_run,
        clean: request.clean,
        note_count: plan.notes.len(),
        page_count: rendered_notes.len()
            + file_list
                .iter()
                .filter(|path| path.ends_with("index.html"))
                .count(),
        asset_count,
        search_enabled: plan.profile.search,
        graph_enabled: plan.profile.graph,
        rss_enabled: plan.profile.rss,
        diagnostics: plan.diagnostics,
        routes: plan.routes,
        rendered_notes,
        files: file_list,
        changed_files: changed_files.into_iter().collect(),
        deleted_files,
    })
}

fn report_site_build_progress<F>(
    progress: &mut F,
    phase: SiteBuildPhase,
    processed: usize,
    total: usize,
    current_path: Option<String>,
) where
    F: FnMut(&SiteBuildProgress),
{
    progress(&SiteBuildProgress {
        phase,
        processed,
        total,
        current_path,
    });
}

#[allow(clippy::too_many_lines)]
pub fn build_frontend_bundle(
    paths: &VaultPaths,
    request: &FrontendBundleRequest,
) -> Result<FrontendBundleBuildReport, AppError> {
    let plan = plan_site(
        paths,
        request.profile.as_deref(),
        Some(request.output_dir.as_path()),
    )?;
    if request.clean && !request.dry_run && plan.profile.output_dir.exists() {
        fs::remove_dir_all(&plan.profile.output_dir).map_err(AppError::operation)?;
    }

    let output_dir = plan.profile.output_dir.clone();
    let SiteRenderOutcome {
        mut rendered_notes,
        search_text_by_path,
        next_state: _,
        rendered_count: _,
    } = render_site_notes(paths, &plan, None, &mut |_| {})?;
    rendered_notes.sort_by(|left, right| left.route.url_path.cmp(&right.route.url_path));
    let cached_asset_state = load_site_asset_copy_state(paths, &plan.profile)?;
    let cached_assets_by_source = cached_asset_state
        .as_ref()
        .map(site_asset_copy_state_by_source);
    let asset_work_items =
        collect_site_asset_copy_work_items(paths, &plan, &rendered_notes, &output_dir)?;

    let context = RenderContext {
        profile: plan.profile.name.clone(),
        site_title: plan.profile.title.clone(),
        language: plan.profile.language.clone(),
        theme: plan.profile.theme.clone(),
        base_url: plan.profile.base_url.clone(),
        deploy_path: plan.profile.deploy_path.clone(),
    };
    let tag_index = build_tag_index(&rendered_notes);
    let navigation_tree = build_navigation_tree(&context.deploy_path, &rendered_notes);
    let note_documents = rendered_notes
        .iter()
        .map(|note| FrontendBundleNoteDocument {
            source_path: note.source_path.clone(),
            title: note.title.clone(),
            excerpt: note.excerpt.clone(),
            description: note.description.clone(),
            canonical_url: note.canonical_url.clone(),
            summary_image: note.summary_image.clone(),
            route: note.route.clone(),
            body_html: note.html.clone(),
            headings: note.headings.clone(),
            tags: note.tags.clone(),
            aliases: note.aliases.clone(),
            outgoing_links: note.outgoing_links.clone(),
            backlinks: note.backlinks.clone(),
            breadcrumbs: note.breadcrumbs.clone(),
            asset_paths: note.asset_paths.clone(),
            embeds: note.embeds.clone(),
            diagnostics: note.diagnostics.clone(),
            file_mtime: note.file_mtime,
        })
        .collect::<Vec<_>>();
    let note_index = note_documents
        .iter()
        .map(|note| FrontendBundleNoteIndexEntry {
            source_path: note.source_path.clone(),
            title: note.title.clone(),
            excerpt: note.excerpt.clone(),
            canonical_url: note.canonical_url.clone(),
            summary_image: note.summary_image.clone(),
            route: note.route.clone(),
            document_path: frontend_bundle_note_document_path(&note.route),
            tags: note.tags.clone(),
        })
        .collect::<Vec<_>>();

    if !request.dry_run {
        fs::create_dir_all(&output_dir).map_err(AppError::operation)?;
    }

    let mut files = BTreeSet::<String>::new();
    let mut changed_files = BTreeSet::<String>::new();
    let mut changed_routes = BTreeSet::<String>::new();
    let mut copied_assets = BTreeSet::<String>::new();
    let mut next_asset_state = Vec::with_capacity(asset_work_items.len());

    for note in &note_documents {
        let relative_path = frontend_bundle_note_document_path(&note.route);
        let payload = render_json_payload(note, request.pretty)?;
        if !request.dry_run && write_output_file(&output_dir.join(&relative_path), &payload)? {
            changed_files.insert(relative_path.clone());
            changed_routes.insert(note.route.url_path.clone());
        }
        files.insert(relative_path);
    }

    for asset in &asset_work_items {
        if !request.dry_run {
            let copied = copy_file_from_vault_with_cache(
                paths,
                &asset.source_path,
                &asset.destination,
                cached_assets_by_source
                    .as_ref()
                    .and_then(|assets| assets.get(asset.source_key.as_str()).copied()),
            )?;
            if copied.changed {
                changed_files.insert(asset.relative_output_path.clone());
            }
            next_asset_state.push(copied.state);
        }
        files.insert(asset.relative_output_path.clone());
        copied_assets.insert(asset.relative_output_path.clone());
    }

    let route_manifest_path = "assets/route-manifest.json".to_string();
    let navigation_tree_path = frontend_bundle_navigation_tree_path().to_string();
    let hover_manifest_path = "assets/hover-previews.json".to_string();
    let recent_manifest_path = "assets/recent-notes.json".to_string();
    let related_manifest_path = "assets/related-notes.json".to_string();
    let note_index_path = frontend_bundle_note_index_path().to_string();
    let contract_path = frontend_bundle_root_contract_path().to_string();
    let schema_path = frontend_bundle_schema_path().to_string();
    let types_path = frontend_bundle_types_path().to_string();
    let invalidation_path = frontend_bundle_invalidation_path().to_string();

    write_serialized_json_output(
        &output_dir,
        &route_manifest_path,
        &plan.routes,
        request.pretty,
        request.dry_run,
        &mut files,
        &mut changed_files,
    )?;
    write_serialized_json_output(
        &output_dir,
        &navigation_tree_path,
        &navigation_tree,
        request.pretty,
        request.dry_run,
        &mut files,
        &mut changed_files,
    )?;
    let hover_manifest = build_hover_manifest(&rendered_notes);
    write_serialized_json_output(
        &output_dir,
        &hover_manifest_path,
        &hover_manifest,
        request.pretty,
        request.dry_run,
        &mut files,
        &mut changed_files,
    )?;
    let recent_manifest = build_recent_manifest(&rendered_notes);
    write_serialized_json_output(
        &output_dir,
        &recent_manifest_path,
        &recent_manifest,
        request.pretty,
        request.dry_run,
        &mut files,
        &mut changed_files,
    )?;
    let related_manifest = build_related_manifest(&rendered_notes, &tag_index);
    write_serialized_json_output(
        &output_dir,
        &related_manifest_path,
        &related_manifest,
        request.pretty,
        request.dry_run,
        &mut files,
        &mut changed_files,
    )?;
    write_serialized_json_output(
        &output_dir,
        &note_index_path,
        &note_index,
        request.pretty,
        request.dry_run,
        &mut files,
        &mut changed_files,
    )?;

    let search_index_path = if plan.profile.search {
        let relative_path = "assets/search-index.json".to_string();
        let search_index = build_search_index(&rendered_notes, &search_text_by_path);
        write_serialized_json_output(
            &output_dir,
            &relative_path,
            &search_index,
            request.pretty,
            request.dry_run,
            &mut files,
            &mut changed_files,
        )?;
        Some(relative_path)
    } else {
        None
    };
    let graph_path = if plan.profile.graph {
        let relative_path = "assets/graph.json".to_string();
        let graph_json = build_graph_asset(paths, &rendered_notes)?;
        if !request.dry_run && write_output_file(&output_dir.join(&relative_path), &graph_json)? {
            changed_files.insert(relative_path.clone());
        }
        files.insert(relative_path.clone());
        Some(relative_path)
    } else {
        None
    };

    let contract = FrontendBundleContract {
        contract: FrontendBundleContractInfo {
            name: frontend_bundle_contract_name().to_string(),
            version: 1,
        },
        profile: FrontendBundleProfile {
            name: plan.profile.name.clone(),
            title: plan.profile.title.clone(),
            deploy_path: plan.profile.deploy_path.clone(),
            base_url: plan.profile.base_url.clone(),
            language: plan.profile.language.clone(),
            theme: plan.profile.theme.clone(),
            search: plan.profile.search,
            graph: plan.profile.graph,
            backlinks: plan.profile.backlinks,
            rss: plan.profile.rss,
            shell: FrontendBundleShell {
                reader_mode: plan.profile.shell.reader_mode,
                default_palette: site_palette_mode_name(plan.profile.shell.default_palette)
                    .to_string(),
                left_rail: plan.profile.shell.left_rail,
                right_rail: plan.profile.shell.right_rail,
            },
            navigation: FrontendBundleNavigation {
                explorer: plan.profile.navigation.explorer,
                folder_click: site_folder_click_name(plan.profile.navigation.folder_click)
                    .to_string(),
                default_folder_state: site_folder_state_name(
                    plan.profile.navigation.default_folder_state,
                )
                .to_string(),
                use_saved_state: plan.profile.navigation.use_saved_state,
                show_home: plan.profile.navigation.show_home,
                show_recent: plan.profile.navigation.show_recent,
                show_folders: plan.profile.navigation.show_folders,
                show_tags: plan.profile.navigation.show_tags,
                show_graph: plan.profile.navigation.show_graph,
            },
            modules: FrontendBundleModules {
                toc: plan.profile.modules.toc,
                graph: plan.profile.modules.graph,
                backlinks: plan.profile.modules.backlinks,
                outgoing_links: plan.profile.modules.outgoing_links,
            },
        },
        context: context.clone(),
        note_count: note_documents.len(),
        diagnostics: plan.diagnostics.clone(),
        routes: plan.routes.clone(),
        notes: note_index.clone(),
        artifacts: FrontendBundleArtifactPaths {
            route_manifest: route_manifest_path.clone(),
            navigation_tree: navigation_tree_path.clone(),
            hover_previews: hover_manifest_path.clone(),
            recent_notes: recent_manifest_path.clone(),
            related_notes: related_manifest_path.clone(),
            note_index: note_index_path.clone(),
            search_index: search_index_path.clone(),
            graph: graph_path.clone(),
            invalidation: invalidation_path.clone(),
            schema: schema_path.clone(),
            typescript: types_path.clone(),
            copied_assets: copied_assets.iter().cloned().collect(),
        },
    };
    write_serialized_json_output(
        &output_dir,
        &contract_path,
        &contract,
        request.pretty,
        request.dry_run,
        &mut files,
        &mut changed_files,
    )?;

    files.insert(schema_path.clone());
    if !request.dry_run
        && write_output_file(
            &output_dir.join(&schema_path),
            &frontend_bundle_contract_schema_json(),
        )?
    {
        changed_files.insert(schema_path.clone());
    }
    files.insert(types_path.clone());
    if !request.dry_run
        && write_output_file(
            &output_dir.join(&types_path),
            frontend_bundle_typescript_definitions(),
        )?
    {
        changed_files.insert(types_path.clone());
    }
    files.insert(invalidation_path.clone());

    if !request.dry_run {
        save_site_asset_copy_state(
            paths,
            &plan.profile,
            &SiteAssetCopyState {
                version: SITE_ASSET_COPY_STATE_VERSION,
                assets: next_asset_state,
            },
        )?;
    }

    let deleted_files = if request.dry_run {
        Vec::new()
    } else {
        remove_stale_output_files(&output_dir, &files)?
    };
    let mut deleted_routes = BTreeSet::<String>::new();
    let mut deleted_assets = BTreeSet::<String>::new();
    for deleted in &deleted_files {
        if let Some(url_path) =
            frontend_bundle_route_from_document_path(deleted, &plan.profile.deploy_path)
        {
            deleted_routes.insert(url_path);
        } else {
            deleted_assets.insert(deleted.clone());
        }
    }

    let mut changed_assets = changed_files
        .iter()
        .filter(|path| !is_frontend_bundle_note_document(path))
        .cloned()
        .collect::<BTreeSet<_>>();
    let invalidation = FrontendBundleInvalidationReport {
        changed_files: changed_files.iter().cloned().collect(),
        deleted_files: deleted_files.clone(),
        changed_routes: changed_routes.iter().cloned().collect(),
        deleted_routes: deleted_routes.iter().cloned().collect(),
        changed_assets: changed_assets.iter().cloned().collect(),
        deleted_assets: deleted_assets.iter().cloned().collect(),
    };
    let stable_invalidation = FrontendBundleInvalidationReport {
        changed_files: Vec::new(),
        deleted_files: Vec::new(),
        changed_routes: Vec::new(),
        deleted_routes: Vec::new(),
        changed_assets: Vec::new(),
        deleted_assets: Vec::new(),
    };
    if !request.dry_run
        && write_output_file(
            &output_dir.join(&invalidation_path),
            &render_json_payload(&stable_invalidation, request.pretty)?,
        )?
    {
        changed_files.insert(invalidation_path.clone());
        changed_assets.insert(invalidation_path.clone());
    }

    Ok(FrontendBundleBuildReport {
        profile: plan.profile.name,
        output_dir: display_path(&output_dir),
        deploy_path: plan.profile.deploy_path,
        dry_run: request.dry_run,
        clean: request.clean,
        note_count: note_documents.len(),
        asset_count: copied_assets.len(),
        diagnostics: plan.diagnostics,
        routes: plan.routes,
        note_documents,
        contract,
        invalidation: FrontendBundleInvalidationReport {
            changed_files: changed_files.iter().cloned().collect(),
            deleted_files,
            changed_routes: changed_routes.into_iter().collect(),
            deleted_routes: deleted_routes.into_iter().collect(),
            changed_assets: changed_assets.into_iter().collect(),
            deleted_assets: deleted_assets.into_iter().collect(),
        },
        files: files.into_iter().collect(),
        changed_files: changed_files.into_iter().collect(),
        deleted_files: invalidation.deleted_files,
    })
}

fn available_site_profile_names(paths: &VaultPaths) -> Vec<String> {
    let config = load_vault_config(paths).config;
    let mut names = config.site.profiles.keys().cloned().collect::<Vec<_>>();
    if names.is_empty() {
        names.push("default".to_string());
    }
    names.sort();
    names
}

fn resolve_site_profile(
    paths: &VaultPaths,
    requested_name: Option<&str>,
    output_override: Option<&Path>,
) -> Result<ResolvedSiteProfile, AppError> {
    let config = load_vault_config(paths).config;
    let requested_name = requested_name.unwrap_or_else(|| {
        if config.site.profiles.contains_key("public") {
            "public"
        } else if config.site.profiles.len() == 1 {
            config
                .site
                .profiles
                .keys()
                .next()
                .map_or("default", String::as_str)
        } else {
            "default"
        }
    });
    let (raw, implicit) = if let Some(profile) = config.site.profiles.get(requested_name) {
        (profile.clone(), false)
    } else if config.site.profiles.is_empty() && requested_name == "default" {
        (SiteProfileConfig::default(), true)
    } else {
        return Err(AppError::operation(format!(
            "unknown site profile `{requested_name}`"
        )));
    };
    if raw.include_query.is_some() && raw.include_query_json.is_some() {
        return Err(AppError::operation(format!(
            "site profile `{requested_name}` must set only one of `include_query` or `include_query_json`"
        )));
    }
    let output_dir = output_override
        .map(Path::to_path_buf)
        .or_else(|| raw.output_dir.clone())
        .unwrap_or_else(|| PathBuf::from(format!(".vulcan/site/{requested_name}")));
    let output_dir = if output_dir.is_absolute() {
        output_dir
    } else {
        paths.vault_root().join(output_dir)
    };
    let theme = raw.theme.clone().unwrap_or_else(|| "default".to_string());
    let theme_overrides = resolve_site_theme(paths, &theme)?;
    let mut extra_css = theme_overrides.css_assets.clone();
    extra_css.extend(raw.extra_css.clone());
    let mut extra_js = theme_overrides.js_assets.clone();
    extra_js.extend(raw.extra_js.clone());
    let search = raw.search.unwrap_or(true);
    let graph = raw.graph.unwrap_or(true);
    let backlinks = raw.backlinks.unwrap_or(true);
    let shell = ResolvedSiteShell {
        reader_mode: raw.shell.reader_mode.unwrap_or(true),
        default_palette: raw
            .shell
            .default_palette
            .unwrap_or(SitePaletteModeConfig::System),
        left_rail: raw.shell.left_rail.unwrap_or(true),
        right_rail: raw.shell.right_rail.unwrap_or(true),
    };
    let navigation = ResolvedSiteNavigation {
        explorer: raw.navigation.explorer.unwrap_or(true),
        folder_click: raw
            .navigation
            .folder_click
            .unwrap_or(SiteFolderClickBehaviorConfig::Collapse),
        default_folder_state: raw
            .navigation
            .default_folder_state
            .unwrap_or(SiteExplorerFolderStateConfig::Collapsed),
        use_saved_state: raw.navigation.use_saved_state.unwrap_or(true),
        show_home: raw.navigation.show_home.unwrap_or(true),
        show_recent: raw.navigation.show_recent.unwrap_or(true),
        show_folders: raw.navigation.show_folders.unwrap_or(true),
        show_tags: raw.navigation.show_tags.unwrap_or(true),
        show_graph: raw.navigation.show_graph.unwrap_or(graph),
    };
    let modules = ResolvedSiteModules {
        toc: raw.modules.toc.unwrap_or(true),
        graph: raw.modules.graph.unwrap_or(graph),
        backlinks: raw.modules.backlinks.unwrap_or(backlinks),
        outgoing_links: raw.modules.outgoing_links.unwrap_or(true),
    };
    Ok(ResolvedSiteProfile {
        name: requested_name.to_string(),
        title: raw
            .title
            .clone()
            .unwrap_or_else(|| requested_name.replace('-', " ")),
        page_title_template: raw
            .page_title_template
            .clone()
            .unwrap_or_else(|| DEFAULT_PAGE_TITLE_TEMPLATE.to_string()),
        output_dir,
        base_url: raw.base_url.clone(),
        deploy_path: normalize_site_deploy_path(raw.deploy_path.as_deref())?,
        home: raw.home.clone(),
        language: raw.language.clone().unwrap_or_else(|| "en".to_string()),
        theme,
        search,
        graph,
        backlinks,
        rss: raw.rss.unwrap_or(false),
        shell,
        navigation,
        modules,
        favicon: raw.favicon.clone(),
        logo: raw.logo.clone(),
        extra_css,
        extra_js,
        include_query: raw.include_query.clone(),
        include_query_json: raw.include_query_json.clone(),
        include_paths: raw.include_paths.clone(),
        include_folders: raw.include_folders.clone(),
        exclude_paths: raw.exclude_paths.clone(),
        exclude_folders: raw.exclude_folders.clone(),
        exclude_tags: raw.exclude_tags.clone(),
        link_policy: raw.link_policy.unwrap_or(SiteLinkPolicyConfig::Warn),
        asset_policy: raw.asset_policy,
        dataview_js: raw.dataview_js.unwrap_or(SiteDataviewJsPolicyConfig::Off),
        raw_html: raw.raw_html.unwrap_or(SiteRawHtmlPolicyConfig::Passthrough),
        theme_overrides,
        content_transform_rules: raw.content_transform_rules.clone(),
        implicit,
    })
}

fn plan_site(
    paths: &VaultPaths,
    profile_name: Option<&str>,
    output_override: Option<&Path>,
) -> Result<SitePlan, AppError> {
    let profile = resolve_site_profile(paths, profile_name, output_override)?;
    let selected = select_site_notes(paths, &profile)?;
    let transforms_active = profile
        .content_transform_rules
        .as_deref()
        .is_some_and(content_transform_rules_have_effective_transforms);
    let (notes, links) = if transforms_active {
        let query = build_profile_query_ast(&profile)?;
        let report = QueryReport {
            query,
            notes: selected.selected.clone(),
        };
        let prepared = prepare_export_data(
            paths,
            &report,
            None,
            profile.content_transform_rules.as_deref(),
        )
        .map_err(AppError::operation)?;
        (
            prepared
                .notes
                .into_iter()
                .map(|document| SitePlanNote {
                    note: document.note,
                    published_source: Some(document.content),
                })
                .collect::<Vec<_>>(),
            prepared.links,
        )
    } else {
        let links = load_export_links_for_notes(paths, &selected.selected)?;
        (
            selected
                .selected
                .iter()
                .cloned()
                .map(|note| SitePlanNote {
                    note,
                    published_source: None,
                })
                .collect::<Vec<_>>(),
            links,
        )
    };
    let routes = plan_note_routes(&notes, &profile.name, &profile.deploy_path);
    let diagnostics = collect_site_diagnostics(paths, &profile, &notes, &links, &routes);
    let config_signature = site_build_config_signature(paths, &profile);
    Ok(SitePlan {
        profile,
        notes,
        links,
        routes,
        diagnostics,
        all_note_signatures: selected.all_note_signatures,
        config_signature,
        transforms_active,
    })
}

fn build_profile_query_ast(profile: &ResolvedSiteProfile) -> Result<QueryAst, AppError> {
    if let Some(query) = profile.include_query.as_deref() {
        return QueryAst::from_dsl(query).map_err(AppError::operation);
    }
    if let Some(query_json) = profile.include_query_json.as_deref() {
        return QueryAst::from_json(query_json).map_err(AppError::operation);
    }
    QueryAst::from_dsl("from notes").map_err(AppError::operation)
}

fn select_site_notes(
    paths: &VaultPaths,
    profile: &ResolvedSiteProfile,
) -> Result<SiteSelectedNotes, AppError> {
    let all_report = execute_query_report(
        paths,
        QueryAst::from_dsl("from notes").map_err(AppError::operation)?,
    )
    .map_err(AppError::operation)?;
    let all_note_signatures = all_report
        .notes
        .iter()
        .map(|note| {
            (
                note.document_path.clone(),
                site_input_signature_for_note(note),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut selected = BTreeSet::<String>::new();
    let has_includes = profile.include_query.is_some()
        || profile.include_query_json.is_some()
        || !profile.include_paths.is_empty()
        || !profile.include_folders.is_empty();
    if !has_includes {
        selected.extend(
            all_report
                .notes
                .iter()
                .map(|note| note.document_path.clone()),
        );
    }
    if let Some(query) = profile.include_query.as_deref() {
        let report = execute_query_report(
            paths,
            QueryAst::from_dsl(query).map_err(AppError::operation)?,
        )
        .map_err(AppError::operation)?;
        selected.extend(report.notes.into_iter().map(|note| note.document_path));
    }
    if let Some(query_json) = profile.include_query_json.as_deref() {
        let report = execute_query_report(
            paths,
            QueryAst::from_json(query_json).map_err(AppError::operation)?,
        )
        .map_err(AppError::operation)?;
        selected.extend(report.notes.into_iter().map(|note| note.document_path));
    }
    for path in &profile.include_paths {
        selected.insert(
            resolve_note_reference(paths, path)
                .map_err(AppError::operation)?
                .path,
        );
    }
    for pattern in &profile.include_folders {
        for note in &all_report.notes {
            if path_matches_selector(&note.document_path, pattern) {
                selected.insert(note.document_path.clone());
            }
        }
    }

    let excluded_paths = profile
        .exclude_paths
        .iter()
        .map(|path| {
            resolve_note_reference(paths, path)
                .map_or_else(|_| path.clone(), |reference| reference.path)
        })
        .collect::<HashSet<_>>();
    let excluded_tags = profile
        .exclude_tags
        .iter()
        .map(|tag| normalize_tag(tag))
        .collect::<HashSet<_>>();

    let mut notes = all_report
        .notes
        .into_iter()
        .filter(|note| selected.contains(&note.document_path))
        .filter(|note| !excluded_paths.contains(&note.document_path))
        .filter(|note| {
            !profile
                .exclude_folders
                .iter()
                .any(|pattern| path_matches_selector(&note.document_path, pattern))
        })
        .filter(|note| {
            note.tags
                .iter()
                .map(|tag| normalize_tag(tag))
                .all(|tag| !excluded_tags.contains(&tag))
        })
        .collect::<Vec<_>>();
    notes.sort_by(|left, right| left.document_path.cmp(&right.document_path));
    Ok(SiteSelectedNotes {
        selected: notes,
        all_note_signatures,
    })
}

fn plan_note_routes(
    notes: &[SitePlanNote],
    profile_name: &str,
    deploy_path: &str,
) -> Vec<SiteRoute> {
    let mut routes = notes
        .iter()
        .map(|document| {
            let title = note_title(&document.note, profile_name, None);
            let slug = note_route_slug(&document.note, profile_name);
            let route_segments = slug
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(slugify_segment)
                .collect::<Vec<_>>();
            let route_path = if route_segments.is_empty() {
                "notes/index".to_string()
            } else {
                format!("notes/{}/index", route_segments.join("/"))
            };
            let url_path = if route_segments.is_empty() {
                prefixed_site_path(deploy_path, "/notes/")
            } else {
                prefixed_site_path(
                    deploy_path,
                    &format!("/notes/{}/", route_segments.join("/")),
                )
            };
            SiteRoute {
                kind: "note".to_string(),
                source_path: Some(document.note.document_path.clone()),
                title,
                slug,
                url_path,
                output_path: format!("{route_path}.html"),
            }
        })
        .collect::<Vec<_>>();
    routes.sort_by(|left, right| left.url_path.cmp(&right.url_path));
    routes
}

fn collect_site_diagnostics(
    paths: &VaultPaths,
    profile: &ResolvedSiteProfile,
    notes: &[SitePlanNote],
    links: &[ExportLinkRecord],
    routes: &[SiteRoute],
) -> Vec<SiteDiagnostic> {
    let published = notes
        .iter()
        .map(|note| note.note.document_path.as_str())
        .collect::<HashSet<_>>();
    let mut diagnostics = Vec::<SiteDiagnostic>::new();
    let mut route_sources = HashMap::<String, String>::new();

    for route in routes {
        if let Some(source_path) = route.source_path.as_ref() {
            if let Some(previous) =
                route_sources.insert(route.url_path.clone(), source_path.clone())
            {
                diagnostics.push(SiteDiagnostic {
                    level: "error".to_string(),
                    kind: "route_collision".to_string(),
                    source_path: Some(source_path.clone()),
                    message: format!(
                        "route `{}` is produced by both `{previous}` and `{}`",
                        route.url_path, source_path
                    ),
                });
            }
        }
    }

    if let Some(home) = profile.home.as_deref() {
        match resolve_note_reference(paths, home) {
            Ok(reference) if !published.contains(reference.path.as_str()) => {
                diagnostics.push(SiteDiagnostic {
                    level: "warn".to_string(),
                    kind: "home_unpublished".to_string(),
                    source_path: Some(reference.path),
                    message: "configured home note is outside the published subset".to_string(),
                });
            }
            Err(error) => diagnostics.push(SiteDiagnostic {
                level: "warn".to_string(),
                kind: "home_missing".to_string(),
                source_path: None,
                message: error.to_string(),
            }),
            Ok(_) => {}
        }
    }

    for link in links {
        let is_markdown_target = link
            .resolved_target_extension
            .as_deref()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
        if is_markdown_target {
            match link.resolved_target_path.as_deref() {
                Some(target) if !published.contains(target) => diagnostics.push(SiteDiagnostic {
                    level: site_link_policy_level(profile.link_policy).to_string(),
                    kind: "unpublished_link_target".to_string(),
                    source_path: Some(link.source_document_path.clone()),
                    message: format!(
                        "link `{}` points to excluded note `{target}`",
                        link.raw_text
                    ),
                }),
                None if link.target_path_candidate.is_some() => diagnostics.push(SiteDiagnostic {
                    level: site_link_policy_level(profile.link_policy).to_string(),
                    kind: "unresolved_note_link".to_string(),
                    source_path: Some(link.source_document_path.clone()),
                    message: format!("link `{}` could not be resolved", link.raw_text),
                }),
                _ => {}
            }
        } else if is_internal_asset_link(link) && link.resolved_target_path.is_none() {
            diagnostics.push(SiteDiagnostic {
                level: if profile.asset_policy.mode == SiteAssetPolicyModeConfig::ErrorOnMissing {
                    "error"
                } else {
                    "warn"
                }
                .to_string(),
                kind: "missing_asset".to_string(),
                source_path: Some(link.source_document_path.clone()),
                message: format!("asset reference `{}` could not be resolved", link.raw_text),
            });
        }
    }

    diagnostics
}

fn render_site_notes<F>(
    paths: &VaultPaths,
    plan: &SitePlan,
    cached_state: Option<&SiteBuildState>,
    progress: &mut F,
) -> Result<SiteRenderOutcome, AppError>
where
    F: FnMut(&SiteBuildProgress),
{
    let shared = build_site_render_shared(plan);
    let reusable_state =
        cached_state.filter(|state| site_build_state_matches_plan(plan, state, &shared.route_map));
    let cached_by_path = reusable_state.map(site_build_state_notes_by_path);
    let current_link_hashes = current_site_link_hashes(plan, &shared)?;

    let notes_to_render = if let Some(state) = reusable_state {
        let cached_by_path = cached_by_path
            .as_ref()
            .expect("reusable state should always have a path map");
        // Reuse cached HTML whenever the selected set and routes are stable, then
        // conservatively invalidate notes whose output may be affected by source,
        // link-resolution, backlink, listing, embed, or vault-query changes.
        let direct_changed = plan
            .notes
            .iter()
            .filter(|note| {
                cached_by_path
                    .get(note.note.document_path.as_str())
                    .is_none_or(|cached| {
                        cached.signature != site_input_signature_for_note(&note.note)
                    })
            })
            .map(|note| note.note.document_path.clone())
            .collect::<HashSet<_>>();
        let link_changed = plan
            .notes
            .iter()
            .filter(|note| {
                cached_by_path
                    .get(note.note.document_path.as_str())
                    .zip(current_link_hashes.get(&note.note.document_path))
                    .is_some_and(|(cached, current)| {
                        cached.dependencies.link_state_hash != current.as_str()
                    })
            })
            .map(|note| note.note.document_path.clone())
            .collect::<HashSet<_>>();
        let current_outgoing = current_outgoing_links_by_source(plan, &shared);
        let backlink_dependents = collect_backlink_dependents(
            &direct_changed,
            &link_changed,
            cached_by_path,
            &current_outgoing,
        );
        let tag_folder_dependents =
            collect_tag_and_folder_dependents(&direct_changed, plan, cached_by_path);
        let (base_changed, base_changed_files) = collect_base_dependency_changes(paths, state)?;
        let query_dependents = if !plan.all_note_signatures.eq(&state.all_note_signatures)
            || !base_changed_files.is_empty()
        {
            state
                .notes
                .iter()
                .filter(|note| note.dependencies.has_vault_queries)
                .map(|note| note.source_path.clone())
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        expand_embed_dependents(
            &direct_changed
                .into_iter()
                .chain(link_changed)
                .chain(backlink_dependents)
                .chain(tag_folder_dependents)
                .chain(base_changed)
                .chain(query_dependents)
                .collect::<HashSet<_>>(),
            state,
        )
    } else {
        plan.notes
            .iter()
            .map(|note| note.note.document_path.clone())
            .collect::<HashSet<_>>()
    };

    let total = notes_to_render.len();
    report_site_build_progress(progress, SiteBuildPhase::RenderingNotes, 0, total, None);
    let html_renderer = (!notes_to_render.is_empty()).then(|| VaultHtmlRenderer::load(paths));
    let vault_config = (!notes_to_render.is_empty()).then(|| load_vault_config(paths).config);
    let mut rendered_notes = Vec::with_capacity(plan.notes.len());
    let mut next_state_notes = Vec::with_capacity(plan.notes.len());
    let mut search_text_by_path = HashMap::with_capacity(plan.notes.len());
    let mut processed = 0_usize;

    for note in &plan.notes {
        let source_path = note.note.document_path.as_str();
        let should_render = notes_to_render.contains(source_path);
        if !should_render {
            if let Some(cached) = cached_by_path
                .as_ref()
                .and_then(|cached| cached.get(source_path))
            {
                rendered_notes.push(cached.rendered.clone());
                search_text_by_path.insert(cached.source_path.clone(), cached.search_text.clone());
                next_state_notes.push((*cached).clone());
                continue;
            }
        }

        let source = load_site_plan_note_source(paths, note)?;
        let rendered = render_site_plan_note(
            paths,
            plan,
            note,
            source.as_ref(),
            &shared,
            html_renderer
                .as_ref()
                .expect("renderer should exist whenever notes are rendered"),
            vault_config
                .as_ref()
                .expect("config should exist whenever notes are rendered"),
            current_link_hashes
                .get(source_path)
                .expect("every note should have a link hash"),
        )?;
        processed += 1;
        report_site_build_progress(
            progress,
            SiteBuildPhase::RenderingNotes,
            processed,
            total,
            Some(source_path.to_string()),
        );
        rendered_notes.push(rendered.rendered.clone());
        search_text_by_path.insert(rendered.source_path.clone(), rendered.search_text.clone());
        next_state_notes.push(rendered);
    }

    next_state_notes.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    Ok(SiteRenderOutcome {
        rendered_notes,
        search_text_by_path,
        next_state: (!plan.transforms_active).then_some(SiteBuildState {
            version: SITE_BUILD_STATE_VERSION,
            config_signature: plan.config_signature.clone(),
            all_note_signatures: plan.all_note_signatures.clone(),
            notes: next_state_notes,
        }),
        rendered_count: processed,
    })
}

fn build_site_render_shared(plan: &SitePlan) -> SiteRenderShared<'_> {
    let route_map = plan
        .routes
        .iter()
        .filter_map(|route| {
            route
                .source_path
                .as_ref()
                .map(|path| (path.clone(), route.clone()))
        })
        .collect::<HashMap<_, _>>();
    let published_paths = route_map.keys().cloned().collect::<HashSet<_>>();
    let asset_hrefs = collect_asset_links(&plan.links, &plan.profile.deploy_path);
    let mut note_hrefs = HashMap::new();
    for (path, route) in &route_map {
        for alias in note_route_aliases(path) {
            note_hrefs.insert(alias, route.url_path.clone());
        }
    }
    let tag_hrefs = build_tag_href_map(&plan.notes, &plan.profile.deploy_path);
    let links_by_source = links_by_source(&plan.links);
    let backlinks = backlinks_by_target(&plan.links, &published_paths);
    let link_targets = HtmlLinkTargets {
        note_hrefs,
        asset_hrefs: asset_hrefs.clone(),
        tag_hrefs,
    };
    SiteRenderShared {
        route_map,
        published_paths,
        asset_hrefs,
        link_targets,
        links_by_source,
        backlinks,
    }
}

fn load_site_plan_note_source<'a>(
    paths: &VaultPaths,
    note: &'a SitePlanNote,
) -> Result<Cow<'a, str>, AppError> {
    if let Some(source) = note.published_source.as_deref() {
        return Ok(Cow::Borrowed(source));
    }
    fs::read_to_string(paths.vault_root().join(&note.note.document_path))
        .map(Cow::Owned)
        .map_err(AppError::operation)
}

fn render_site_plan_note(
    paths: &VaultPaths,
    plan: &SitePlan,
    note: &SitePlanNote,
    source: &str,
    shared: &SiteRenderShared<'_>,
    html_renderer: &VaultHtmlRenderer,
    vault_config: &vulcan_core::config::VaultConfig,
    link_state_hash: &str,
) -> Result<SiteBuildStateNote, AppError> {
    let route = shared
        .route_map
        .get(&note.note.document_path)
        .ok_or_else(|| AppError::operation("missing note route during site render"))?;
    let source_links = shared
        .links_by_source
        .get(note.note.document_path.as_str())
        .map_or(&[][..], Vec::as_slice);
    let adjusted = apply_link_policy_to_source(
        source,
        source_links,
        &shared.published_paths,
        plan.profile.link_policy,
    );
    let rendered = html_renderer.render(
        adjusted.as_ref(),
        &HtmlRenderOptions {
            source_path: Some(&note.note.document_path),
            full_document: true,
            link_targets: Some(&shared.link_targets),
            dataview_js_policy: match plan.profile.dataview_js {
                SiteDataviewJsPolicyConfig::Off => HtmlDataviewJsPolicy::Off,
                SiteDataviewJsPolicyConfig::Static => HtmlDataviewJsPolicy::Static,
            },
            raw_html_policy: match plan.profile.raw_html {
                SiteRawHtmlPolicyConfig::Passthrough => HtmlRawHtmlPolicy::Passthrough,
                SiteRawHtmlPolicyConfig::Sanitize => HtmlRawHtmlPolicy::Sanitize,
                SiteRawHtmlPolicyConfig::Strip => HtmlRawHtmlPolicy::Strip,
            },
            max_embed_depth: 4,
        },
    );
    let diagnostics = rendered.diagnostics.clone();
    let title = note_title(&note.note, &plan.profile.name, rendered.title.as_deref());
    let excerpt = excerpt_from_markdown(adjusted.as_ref());
    let search_text = search_text_from_markdown(adjusted.as_ref());
    let description = frontmatter_override(&note.note, &plan.profile.name, "description")
        .unwrap_or_else(|| excerpt.clone());
    let canonical_url = frontmatter_override(&note.note, &plan.profile.name, "canonical_url");
    let summary_image = frontmatter_override(&note.note, &plan.profile.name, "summary_image");
    let hidden_modules = frontmatter_string_list(&note.note, &plan.profile.name, "hide_modules")
        .into_iter()
        .map(|module| module.replace('-', "_"))
        .collect::<Vec<_>>();
    let rendered_note = RenderedNote {
        source_path: note.note.document_path.clone(),
        title,
        excerpt,
        description,
        canonical_url,
        summary_image,
        route: route.clone(),
        html: rendered.html,
        headings: rendered.headings,
        tags: note.note.tags.clone(),
        aliases: note.note.aliases.clone(),
        outgoing_links: collect_note_outgoing_links(source_links, &shared.published_paths),
        backlinks: shared
            .backlinks
            .get(&note.note.document_path)
            .cloned()
            .unwrap_or_default(),
        hidden_modules,
        breadcrumbs: breadcrumbs_for_path(&note.note.document_path),
        asset_paths: collect_note_asset_paths(source_links),
        embeds: collect_note_embeds(source_links, &note.note.document_path, shared),
        diagnostics,
        file_mtime: note.note.file_mtime,
    };
    let dependencies = build_site_note_dependencies(
        paths,
        source,
        source_links,
        vault_config,
        plan.profile.dataview_js,
        link_state_hash,
    )?;
    Ok(SiteBuildStateNote {
        source_path: note.note.document_path.clone(),
        signature: site_input_signature_for_note(&note.note),
        rendered: rendered_note,
        search_text,
        page_output_signature: None,
        dependencies,
    })
}

fn build_site_note_dependencies(
    paths: &VaultPaths,
    source: &str,
    source_links: &[&ExportLinkRecord],
    vault_config: &vulcan_core::config::VaultConfig,
    dataview_js_policy: SiteDataviewJsPolicyConfig,
    link_state_hash: &str,
) -> Result<SiteNoteDependencies, AppError> {
    let parsed = parse_document(source, vault_config);
    let has_query_blocks = parsed
        .dataview_blocks
        .iter()
        .any(|block| block.language != "dataviewjs")
        || !parsed.tasks_blocks.is_empty()
        || (dataview_js_policy == SiteDataviewJsPolicyConfig::Static
            && parsed
                .dataview_blocks
                .iter()
                .any(|block| block.language == "dataviewjs"));

    let note_embeds = source_links
        .iter()
        .filter(|link| link.link_kind.eq_ignore_ascii_case("embed"))
        .filter(|link| {
            link.resolved_target_extension
                .as_deref()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        })
        .filter_map(|link| link.resolved_target_path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut base_embeds = source_links
        .iter()
        .filter(|link| link.link_kind.eq_ignore_ascii_case("embed"))
        .filter_map(|link| {
            let target = link
                .resolved_target_path
                .as_deref()
                .or(link.target_path_candidate.as_deref())?;
            Path::new(target)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
                .then_some(normalize_relative_path(target))
        })
        .map(|path| {
            let signature = site_input_signature_for_relative_path(paths, &path)?;
            Ok(SiteFileDependency {
                path: display_path(&path),
                signature,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    base_embeds.sort_by(|left, right| left.path.cmp(&right.path));
    base_embeds.dedup_by(|left, right| left.path == right.path);
    let has_vault_queries = has_query_blocks || !base_embeds.is_empty();

    Ok(SiteNoteDependencies {
        link_state_hash: link_state_hash.to_string(),
        note_embeds,
        base_embeds,
        has_vault_queries,
    })
}

fn current_site_link_hashes(
    plan: &SitePlan,
    shared: &SiteRenderShared<'_>,
) -> Result<HashMap<String, String>, AppError> {
    plan.notes
        .iter()
        .map(|note| {
            let source_links = shared
                .links_by_source
                .get(note.note.document_path.as_str())
                .map_or(&[][..], Vec::as_slice);
            Ok((
                note.note.document_path.clone(),
                site_link_state_hash(source_links)?,
            ))
        })
        .collect()
}

fn current_outgoing_links_by_source(
    plan: &SitePlan,
    shared: &SiteRenderShared<'_>,
) -> HashMap<String, Vec<String>> {
    plan.notes
        .iter()
        .map(|note| {
            let source_links = shared
                .links_by_source
                .get(note.note.document_path.as_str())
                .map_or(&[][..], Vec::as_slice);
            (
                note.note.document_path.clone(),
                collect_note_outgoing_links(source_links, &shared.published_paths),
            )
        })
        .collect()
}

fn collect_backlink_dependents(
    direct_changed: &HashSet<String>,
    link_changed: &HashSet<String>,
    cached_by_path: &HashMap<&str, &SiteBuildStateNote>,
    current_outgoing: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    direct_changed
        .iter()
        .chain(link_changed.iter())
        .flat_map(|source_path| {
            let previous = cached_by_path
                .get(source_path.as_str())
                .into_iter()
                .flat_map(|note| note.rendered.outgoing_links.iter().cloned());
            let current = current_outgoing
                .get(source_path)
                .into_iter()
                .flat_map(|links| links.iter().cloned());
            previous.chain(current).collect::<Vec<_>>()
        })
        .collect()
}

fn collect_tag_and_folder_dependents(
    direct_changed: &HashSet<String>,
    plan: &SitePlan,
    cached_by_path: &HashMap<&str, &SiteBuildStateNote>,
) -> HashSet<String> {
    let current_tags = plan
        .notes
        .iter()
        .map(|note| {
            (
                note.note.document_path.clone(),
                note.note
                    .tags
                    .iter()
                    .map(|tag| normalize_tag(tag))
                    .collect::<HashSet<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    let current_folders = plan
        .notes
        .iter()
        .map(|note| {
            (
                note.note.document_path.clone(),
                folder_for_note(&note.note.document_path),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut dependents = HashSet::new();

    for source_path in direct_changed {
        let previous_tags = cached_by_path
            .get(source_path.as_str())
            .map(|note| {
                note.rendered
                    .tags
                    .iter()
                    .map(|tag| normalize_tag(tag))
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let previous_folder = cached_by_path
            .get(source_path.as_str())
            .map(|note| folder_for_note(&note.source_path));
        let current_note_tags = current_tags.get(source_path).cloned().unwrap_or_default();
        let current_folder = current_folders
            .get(source_path)
            .cloned()
            .unwrap_or_default();

        for note in &plan.notes {
            let candidate_path = &note.note.document_path;
            let candidate_tags = current_tags
                .get(candidate_path)
                .expect("every note should have a current tag set");
            let candidate_folder = current_folders
                .get(candidate_path)
                .expect("every note should have a current folder");
            if candidate_tags
                .iter()
                .any(|tag| previous_tags.contains(tag) || current_note_tags.contains(tag))
                || previous_folder.as_deref() == Some(candidate_folder.as_str())
                || current_folder == *candidate_folder
            {
                dependents.insert(candidate_path.clone());
            }
        }
    }

    dependents
}

fn collect_base_dependency_changes(
    paths: &VaultPaths,
    state: &SiteBuildState,
) -> Result<(HashSet<String>, HashSet<String>), AppError> {
    let mut changed_notes = HashSet::new();
    let mut changed_files = HashSet::new();
    for note in &state.notes {
        let mut note_changed = false;
        for dependency in &note.dependencies.base_embeds {
            let current =
                site_input_signature_for_relative_path(paths, Path::new(&dependency.path))?;
            if current != dependency.signature {
                note_changed = true;
                changed_files.insert(dependency.path.clone());
            }
        }
        if note_changed {
            changed_notes.insert(note.source_path.clone());
        }
    }
    Ok((changed_notes, changed_files))
}

fn expand_embed_dependents(changed: &HashSet<String>, state: &SiteBuildState) -> HashSet<String> {
    let mut expanded = changed.clone();
    loop {
        let mut grew = false;
        for note in &state.notes {
            if expanded.contains(&note.source_path) {
                continue;
            }
            if note
                .dependencies
                .note_embeds
                .iter()
                .any(|target| expanded.contains(target))
                && expanded.insert(note.source_path.clone())
            {
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    expanded
}

fn site_build_state_matches_plan(
    plan: &SitePlan,
    state: &SiteBuildState,
    route_map: &HashMap<String, SiteRoute>,
) -> bool {
    if state.version != SITE_BUILD_STATE_VERSION
        || state.config_signature != plan.config_signature
        || state.notes.len() != plan.notes.len()
    {
        return false;
    }
    let cached_by_path = site_build_state_notes_by_path(state);
    plan.notes.iter().all(|note| {
        cached_by_path
            .get(note.note.document_path.as_str())
            .and_then(|cached| {
                route_map
                    .get(&note.note.document_path)
                    .map(|route| cached.rendered.route == *route)
            })
            .unwrap_or(false)
    })
}

fn site_build_state_notes_by_path(state: &SiteBuildState) -> HashMap<&str, &SiteBuildStateNote> {
    state
        .notes
        .iter()
        .map(|note| (note.source_path.as_str(), note))
        .collect()
}

fn site_link_state_hash(source_links: &[&ExportLinkRecord]) -> Result<String, AppError> {
    serde_json::to_vec(source_links)
        .map(|payload| blake3::hash(&payload).to_hex().to_string())
        .map_err(AppError::operation)
}

fn collect_note_outgoing_links(
    source_links: &[&ExportLinkRecord],
    published_paths: &HashSet<String>,
) -> Vec<String> {
    source_links
        .iter()
        .filter_map(|link| link.resolved_target_path.clone())
        .filter(|path| published_paths.contains(path))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn collect_note_asset_paths(source_links: &[&ExportLinkRecord]) -> Vec<String> {
    source_links
        .iter()
        .filter_map(|link| {
            if is_markdown_asset(link) {
                link.resolved_target_path.clone()
            } else {
                None
            }
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn collect_note_embeds(
    source_links: &[&ExportLinkRecord],
    source_path: &str,
    shared: &SiteRenderShared<'_>,
) -> Vec<RenderedEmbed> {
    source_links
        .iter()
        .filter_map(|link| {
            if !link.link_kind.eq_ignore_ascii_case("embed") {
                return None;
            }
            let (target_path, url_path) =
                resolve_embed_target(&shared.route_map, &shared.asset_hrefs, link)?;
            Some(RenderedEmbed {
                kind: link.link_kind.clone(),
                source_path: source_path.to_string(),
                target_path,
                url_path,
            })
        })
        .collect()
}

fn site_input_signature_for_note(note: &NoteRecord) -> SiteInputSignature {
    SiteInputSignature {
        file_mtime: note.file_mtime,
        file_size: note.file_size,
    }
}

fn site_input_signature_for_path(path: &Path) -> Result<Option<SiteInputSignature>, AppError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::operation(error)),
    };
    let file_size = i64::try_from(metadata.len())
        .map_err(|_| AppError::operation("site dependency file size exceeded supported range"))?;
    let file_mtime = metadata
        .modified()
        .ok()
        .and_then(system_time_to_nanos)
        .ok_or_else(|| AppError::operation("failed to read site dependency modification time"))?;
    Ok(Some(SiteInputSignature {
        file_mtime,
        file_size,
    }))
}

fn site_input_signature_for_relative_path(
    paths: &VaultPaths,
    relative: &Path,
) -> Result<Option<SiteInputSignature>, AppError> {
    site_input_signature_for_path(&paths.vault_root().join(relative))
}

fn site_path_signature_matches(
    path: &Path,
    expected: &SiteInputSignature,
) -> Result<bool, AppError> {
    Ok(site_input_signature_for_path(path)?.as_ref() == Some(expected))
}

fn system_time_to_nanos(time: std::time::SystemTime) -> Option<i64> {
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_nanos()).ok()
}

fn site_build_config_signature(paths: &VaultPaths, profile: &ResolvedSiteProfile) -> String {
    let vault_config = load_vault_config(paths).config;
    let payload = format!("{vault_config:?}\n{profile:?}");
    blake3::hash(payload.as_bytes()).to_hex().to_string()
}

fn load_site_build_state(
    paths: &VaultPaths,
    profile: &ResolvedSiteProfile,
) -> Result<Option<SiteBuildState>, AppError> {
    let path = site_build_state_path(paths, profile);
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::operation(error)),
    };
    Ok(serde_json::from_str(&contents).ok())
}

fn load_site_asset_copy_state(
    paths: &VaultPaths,
    profile: &ResolvedSiteProfile,
) -> Result<Option<SiteAssetCopyState>, AppError> {
    let path = site_asset_copy_state_path(paths, profile);
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::operation(error)),
    };
    Ok(serde_json::from_str::<SiteAssetCopyState>(&contents)
        .ok()
        .filter(|state| state.version == SITE_ASSET_COPY_STATE_VERSION))
}

fn save_site_build_state(
    paths: &VaultPaths,
    profile: &ResolvedSiteProfile,
    state: &SiteBuildState,
) -> Result<(), AppError> {
    // Store per-profile render state under `.vulcan/` so future builds can skip
    // unchanged note-page compilation while keeping the output directory clean.
    ensure_vulcan_dir(paths).map_err(AppError::operation)?;
    let path = site_build_state_path(paths, profile);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    let payload = serde_json::to_string_pretty(state).map_err(AppError::operation)?;
    fs::write(path, payload).map_err(AppError::operation)
}

fn save_site_asset_copy_state(
    paths: &VaultPaths,
    profile: &ResolvedSiteProfile,
    state: &SiteAssetCopyState,
) -> Result<(), AppError> {
    ensure_vulcan_dir(paths).map_err(AppError::operation)?;
    let path = site_asset_copy_state_path(paths, profile);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    let payload = serde_json::to_string_pretty(state).map_err(AppError::operation)?;
    fs::write(path, payload).map_err(AppError::operation)
}

fn site_build_state_path(paths: &VaultPaths, profile: &ResolvedSiteProfile) -> PathBuf {
    let hash = blake3::hash(site_profile_cache_key(profile).as_bytes())
        .to_hex()
        .to_string();
    paths.vulcan_dir().join("site-state").join(format!(
        "{}-{}.json",
        slugify_segment(&profile.name),
        &hash[..12]
    ))
}

fn site_asset_copy_state_path(paths: &VaultPaths, profile: &ResolvedSiteProfile) -> PathBuf {
    let hash = blake3::hash(site_profile_cache_key(profile).as_bytes())
        .to_hex()
        .to_string();
    paths.vulcan_dir().join("site-assets").join(format!(
        "{}-{}.json",
        slugify_segment(&profile.name),
        &hash[..12]
    ))
}

fn site_profile_cache_key(profile: &ResolvedSiteProfile) -> String {
    format!("{}\0{}", profile.name, profile.output_dir.display())
}

fn resolve_embed_target(
    route_map: &HashMap<String, SiteRoute>,
    asset_hrefs: &HashMap<String, String>,
    link: &ExportLinkRecord,
) -> Option<(String, String)> {
    let target_path = link.resolved_target_path.clone()?;
    let url_path = if link
        .resolved_target_extension
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("md"))
    {
        route_map.get(&target_path).map_or_else(
            || asset_hrefs.get(&target_path).cloned().unwrap_or_default(),
            |route| route.url_path.clone(),
        )
    } else {
        asset_hrefs.get(&target_path).cloned().unwrap_or_default()
    };
    Some((target_path, url_path))
}

fn site_palette_mode_name(mode: SitePaletteModeConfig) -> &'static str {
    match mode {
        SitePaletteModeConfig::System => "system",
        SitePaletteModeConfig::Light => "light",
        SitePaletteModeConfig::Dark => "dark",
    }
}

fn site_folder_click_name(mode: SiteFolderClickBehaviorConfig) -> &'static str {
    match mode {
        SiteFolderClickBehaviorConfig::Collapse => "collapse",
        SiteFolderClickBehaviorConfig::Link => "link",
    }
}

fn site_folder_state_name(mode: SiteExplorerFolderStateConfig) -> &'static str {
    match mode {
        SiteExplorerFolderStateConfig::Collapsed => "collapsed",
        SiteExplorerFolderStateConfig::Open => "open",
    }
}

// Build a published-only explorer tree. Nested index notes become folder landing pages so the
// left rail can prefer folder notes while still falling back to generated folder listings.
fn build_navigation_tree(deploy_path: &str, notes: &[RenderedNote]) -> Vec<SiteNavigationNode> {
    #[derive(Debug, Default)]
    struct FolderBuilder {
        title: String,
        folder_path: String,
        url: String,
        source_path: Option<String>,
        children: BTreeMap<String, FolderBuilder>,
        notes: Vec<SiteNavigationNode>,
    }

    fn finalize(builder: FolderBuilder) -> Vec<SiteNavigationNode> {
        let mut nodes = builder
            .children
            .into_values()
            .map(|child| {
                let title = child.title.clone();
                let folder_path = child.folder_path.clone();
                let url = child.url.clone();
                let source_path = child.source_path.clone();
                let children = finalize(child);
                SiteNavigationNode {
                    kind: "folder".to_string(),
                    title,
                    id: format!("folder:{folder_path}"),
                    url,
                    children,
                    source_path,
                    folder_path: Some(folder_path),
                }
            })
            .collect::<Vec<_>>();
        nodes.sort_by(|left, right| left.title.to_lowercase().cmp(&right.title.to_lowercase()));
        let mut note_nodes = builder.notes;
        note_nodes
            .sort_by(|left, right| left.title.to_lowercase().cmp(&right.title.to_lowercase()));
        nodes.extend(note_nodes);
        nodes
    }

    let mut root = FolderBuilder::default();
    for note in notes {
        let folder = folder_for_note(&note.source_path);
        let folder_note = folder_note_path(&note.source_path);
        let mut current = &mut root;
        if !folder.is_empty() {
            let mut assembled = String::new();
            for segment in folder.split('/') {
                if !assembled.is_empty() {
                    assembled.push('/');
                }
                assembled.push_str(segment);
                current = current
                    .children
                    .entry(segment.to_string())
                    .or_insert_with(|| FolderBuilder {
                        title: segment.to_string(),
                        folder_path: assembled.clone(),
                        url: folder_page_href(deploy_path, &assembled),
                        source_path: None,
                        children: BTreeMap::new(),
                        notes: Vec::new(),
                    });
            }
        }
        if folder_note.as_deref() == Some(folder.as_str()) {
            current.title.clone_from(&note.title);
            current.url.clone_from(&note.route.url_path);
            current.source_path = Some(note.source_path.clone());
        } else {
            current.notes.push(SiteNavigationNode {
                kind: "note".to_string(),
                title: note.title.clone(),
                id: display_path(&normalize_relative_path(&note.source_path)),
                url: note.route.url_path.clone(),
                children: Vec::new(),
                source_path: Some(note.source_path.clone()),
                folder_path: if folder.is_empty() {
                    None
                } else {
                    Some(folder)
                },
            });
        }
    }
    finalize(root)
}

fn navigation_node_contains_current(
    node: &SiteNavigationNode,
    current_note_path: Option<&str>,
) -> bool {
    current_note_path.is_some_and(|current| {
        node.source_path.as_deref() == Some(current)
            || node
                .children
                .iter()
                .any(|child| navigation_node_contains_current(child, Some(current)))
    })
}

fn render_navigation_tree_html(
    nodes: &[SiteNavigationNode],
    current_note_path: Option<&str>,
    navigation: &ResolvedSiteNavigation,
) -> String {
    if nodes.is_empty() {
        return String::new();
    }
    let items = nodes
        .iter()
        .map(|node| render_navigation_node_html(node, current_note_path, navigation))
        .collect::<String>();
    format!(
        "<ul class=\"site-explorer-tree\" data-site-folder-click=\"{}\" data-site-folder-state=\"{}\" data-site-saved-state=\"{}\">{items}</ul>",
        site_folder_click_name(navigation.folder_click),
        site_folder_state_name(navigation.default_folder_state),
        if navigation.use_saved_state { "true" } else { "false" },
    )
}

fn render_navigation_node_html(
    node: &SiteNavigationNode,
    current_note_path: Option<&str>,
    navigation: &ResolvedSiteNavigation,
) -> String {
    let active = node.source_path.as_deref() == current_note_path;
    match node.kind.as_str() {
        "folder" => {
            let open = navigation.default_folder_state == SiteExplorerFolderStateConfig::Open
                || navigation_node_contains_current(node, current_note_path);
            let folder_path = node.folder_path.as_deref().unwrap_or_default();
            let folder_open_link = if navigation.folder_click
                == SiteFolderClickBehaviorConfig::Collapse
                && node.source_path.is_some()
            {
                format!(
                    "<a class=\"site-explorer-folder-open{}\" href=\"{}\" aria-label=\"Open {} overview\">Open</a>",
                    if active { " is-active" } else { "" },
                    escape_html(&node.url),
                    escape_html(&node.title)
                )
            } else {
                String::new()
            };
            let title_html = if navigation.folder_click == SiteFolderClickBehaviorConfig::Link {
                format!(
                    "<a class=\"site-explorer-folder-link{}\" href=\"{}\">{}</a>",
                    if active { " is-active" } else { "" },
                    escape_html(&node.url),
                    escape_html(&node.title)
                )
            } else {
                format!(
                    "<button class=\"site-explorer-folder-label{}\" type=\"button\" data-site-explorer-folder-label=\"{}\">{}</button>",
                    if active { " is-active" } else { "" },
                    escape_html(folder_path),
                    escape_html(&node.title)
                )
            };
            format!(
                concat!(
                    "<li class=\"site-explorer-folder\" data-site-explorer-folder=\"{}\" data-site-explorer-filter-text=\"{}\">",
                    "<div class=\"site-explorer-folder-row\">",
                    "<button class=\"site-explorer-folder-toggle\" type=\"button\" data-site-explorer-folder-toggle=\"{}\" aria-expanded=\"{}\">▸</button>",
                    "{}{}",
                    "</div>",
                    "<ul class=\"site-explorer-children\" data-site-explorer-folder-body=\"{}\" {}>{}</ul>",
                    "</li>"
                ),
                escape_html(folder_path),
                escape_html(&node.title.to_lowercase()),
                escape_html(folder_path),
                if open { "true" } else { "false" },
                title_html,
                folder_open_link,
                escape_html(folder_path),
                if open { "" } else { "hidden" },
                node.children
                    .iter()
                    .map(|child| render_navigation_node_html(child, current_note_path, navigation))
                    .collect::<String>(),
            )
        }
        _ => format!(
            "<li class=\"site-explorer-note\" data-site-explorer-filter-text=\"{}\"><a class=\"site-explorer-link{}\" href=\"{}\">{}</a></li>",
            escape_html(&node.title.to_lowercase()),
            if active { " is-active" } else { "" },
            escape_html(&node.url),
            escape_html(&node.title)
        ),
    }
}

fn render_toc_module(headings: &[HtmlRenderHeading]) -> Option<SiteShellModule> {
    if headings.is_empty() {
        return None;
    }
    let items = headings
        .iter()
        .map(|heading| {
            format!(
                "<li><a href=\"#{}\">{}</a></li>",
                escape_html(&heading.id),
                escape_html(&heading.text)
            )
        })
        .collect::<String>();
    Some(SiteShellModule {
        id: "toc",
        title: "Contents",
        body_html: format!("<ul class=\"site-panel-list\">{items}</ul>"),
    })
}

fn render_note_links_module(
    deploy_path: &str,
    id: &'static str,
    title: &'static str,
    note_paths: &[String],
    route_urls: &HashMap<String, String>,
) -> Option<SiteShellModule> {
    if note_paths.is_empty() {
        return None;
    }
    let items = note_paths
        .iter()
        .map(|path| {
            let href = route_urls.get(path).cloned().unwrap_or_else(|| {
                prefixed_site_path(
                    deploy_path,
                    &format!("/notes/{}/", slugify_path(trim_markdown_extension(path))),
                )
            });
            format!(
                "<li><a href=\"{}\">{}</a></li>",
                escape_html(&href),
                escape_html(path)
            )
        })
        .collect::<String>();
    Some(SiteShellModule {
        id,
        title,
        body_html: format!("<ul class=\"site-panel-list\">{items}</ul>"),
    })
}

fn render_local_graph_module(note: &RenderedNote) -> SiteShellModule {
    SiteShellModule {
        id: "graph",
        title: "Local graph",
        body_html: format!(
            concat!(
                "<div class=\"site-panel-copy\" data-site-local-graph data-site-note-path=\"{}\">",
                "<div class=\"site-graph-stage\" data-site-graph-canvas=\"local\" data-site-note-path=\"{}\">",
                "<p class=\"site-graph-empty\">Loading local graph…</p>",
                "</div>",
                "<p class=\"site-graph-caption\">Direct published links and backlinks for this note.</p>",
                "</div>"
            ),
            escape_html(&note.source_path),
            escape_html(&note.source_path),
        ),
    }
}

fn render_note_document(
    context: &RenderContext,
    note: &RenderedNote,
    previous: Option<&RenderedNote>,
    next: Option<&RenderedNote>,
    profile: &ResolvedSiteProfile,
    navigation_tree: &[SiteNavigationNode],
    route_urls: &HashMap<String, String>,
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
    folder_index: &BTreeMap<String, Vec<&RenderedNote>>,
    home_note: Option<&RenderedNote>,
) -> String {
    let breadcrumbs = render_breadcrumbs(&note.breadcrumbs);
    let hidden_modules = note
        .hidden_modules
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut modules = Vec::new();
    if profile.modules.toc && !hidden_modules.contains("toc") {
        if let Some(module) = render_toc_module(&note.headings) {
            modules.push(module);
        }
    }
    if profile.modules.graph && profile.graph && !hidden_modules.contains("graph") {
        modules.push(render_local_graph_module(note));
    }
    if profile.modules.backlinks && profile.backlinks && !hidden_modules.contains("backlinks") {
        if let Some(module) = render_note_links_module(
            &context.deploy_path,
            "backlinks",
            "Backlinks",
            &note.backlinks,
            route_urls,
        ) {
            modules.push(module);
        }
    }
    if profile.modules.outgoing_links
        && !hidden_modules.contains("outgoing_links")
        && !hidden_modules.contains("outgoing")
    {
        if let Some(module) = render_note_links_module(
            &context.deploy_path,
            "outgoing",
            "Outgoing links",
            &note.outgoing_links,
            route_urls,
        ) {
            modules.push(module);
        }
    }
    let diagnostics = render_note_diagnostics(&note.diagnostics);
    let prev_next = render_prev_next(previous, next);
    let tags = render_note_tags(&context.deploy_path, &note.tags);
    let canonical_url = note
        .canonical_url
        .as_deref()
        .and_then(|value| normalize_site_metadata_url(context.base_url.as_deref(), value))
        .or_else(|| canonical_url_for_path(context.base_url.as_deref(), &note.route.url_path));
    let summary_image = note
        .summary_image
        .as_deref()
        .and_then(|value| summary_image_meta_url(&context.deploy_path, value))
        .and_then(|value| normalize_site_metadata_url(context.base_url.as_deref(), &value));
    let note_body = render_note_theme_chrome(
        context,
        profile,
        &note.title,
        &note.description,
        canonical_url.as_deref(),
        &note.source_path,
        &note.html,
    );
    let body = format!(
        concat!(
            "<article class=\"site-main\">{}",
            "{}{}{}{}{}{}{}{}",
            "</article>"
        ),
        breadcrumbs,
        tags,
        note_body,
        diagnostics,
        prev_next,
        if note.embeds.is_empty() {
            String::new()
        } else {
            format!(
                "<section class=\"site-callout\"><strong>Embeds:</strong> {}</section>",
                escape_html(
                    &note
                        .embeds
                        .iter()
                        .map(|embed| embed.target_path.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            )
        },
        if home_note.is_some_and(|home| home.source_path == note.source_path) {
            "<section class=\"site-callout\">This note is also published as the site home page.</section>".to_string()
        } else {
            String::new()
        },
        render_folder_summary(&context.deploy_path, note, folder_index),
        render_related_tags(note, tag_index),
    );
    render_document_shell(
        context,
        &note.title,
        &note.description,
        &body,
        navigation_tree,
        &modules,
        profile,
        false,
        canonical_url.as_deref(),
        summary_image.as_deref(),
        Some(&note.source_path),
    )
}

fn render_home_page(
    context: &RenderContext,
    note: &RenderedNote,
    navigation_tree: &[SiteNavigationNode],
    profile: &ResolvedSiteProfile,
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
    folder_index: &BTreeMap<String, Vec<&RenderedNote>>,
) -> String {
    let canonical_url = note
        .canonical_url
        .as_deref()
        .and_then(|value| normalize_site_metadata_url(context.base_url.as_deref(), value))
        .or_else(|| {
            canonical_url_for_path(
                context.base_url.as_deref(),
                &site_root_href(&context.deploy_path),
            )
        });
    let summary_image = note
        .summary_image
        .as_deref()
        .and_then(|value| summary_image_meta_url(&context.deploy_path, value))
        .and_then(|value| normalize_site_metadata_url(context.base_url.as_deref(), &value));
    let note_body = render_note_theme_chrome(
        context,
        profile,
        &context.site_title,
        &note.description,
        canonical_url.as_deref(),
        &note.source_path,
        &note.html,
    );
    let body = format!(
        "<article class=\"site-main\">{}{}{}{}</article>",
        render_note_tags(&context.deploy_path, &note.tags),
        note_body,
        render_folder_summary(&context.deploy_path, note, folder_index),
        render_related_tags(note, tag_index),
    );
    let modules = if profile.modules.toc {
        render_toc_module(&note.headings)
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    render_document_shell(
        context,
        &context.site_title,
        &note.description,
        &body,
        navigation_tree,
        &modules,
        profile,
        false,
        canonical_url.as_deref(),
        summary_image.as_deref(),
        Some(&note.source_path),
    )
}

fn render_recent_page(
    context: &RenderContext,
    notes: &[RenderedNote],
    navigation_tree: &[SiteNavigationNode],
    profile: &ResolvedSiteProfile,
) -> String {
    let cards = build_recent_manifest(notes)
        .into_iter()
        .map(|note| render_card(&note.title, &note.url, &note.excerpt))
        .collect::<String>();
    let body = render_listing_section(
        "Recently updated",
        "Freshly changed notes from this published profile.",
        &[format!("{} note(s)", notes.len())],
        &cards,
    );
    render_generic_page(
        context,
        "Recent notes",
        "Most recently updated published notes.",
        &body,
        navigation_tree,
        profile,
        false,
        &prefixed_site_path(&context.deploy_path, "/recent/"),
    )
}

fn render_folder_pages(
    context: &RenderContext,
    folder_index: &BTreeMap<String, Vec<&RenderedNote>>,
    navigation_tree: &[SiteNavigationNode],
    profile: &ResolvedSiteProfile,
) -> Vec<(String, String)> {
    let mut pages = Vec::new();
    let total_notes = folder_index.values().map(Vec::len).sum::<usize>();
    let overview = folder_index
        .iter()
        .map(|(folder, notes)| {
            render_card(
                folder,
                &folder_page_href(&context.deploy_path, folder),
                &format!("{} published note(s)", notes.len()),
            )
        })
        .collect::<String>();
    pages.push((
        "folders/index.html".to_string(),
        render_generic_page(
            context,
            "Folders",
            "Folder-based views of the published subset.",
            &render_listing_section(
                "Folder explorer",
                "Browse the published subset by folder.",
                &[
                    format!("{} folder(s)", folder_index.len()),
                    format!("{total_notes} note(s)"),
                ],
                &overview,
            ),
            navigation_tree,
            profile,
            false,
            &prefixed_site_path(&context.deploy_path, "/folders/"),
        ),
    ));
    for (folder, notes) in folder_index {
        if folder.is_empty() {
            continue;
        }
        let list = notes
            .iter()
            .map(|note| render_card(&note.title, &note.route.url_path, &note.excerpt))
            .collect::<String>();
        pages.push((
            format!("folders/{}/index.html", slugify_path(folder)),
            render_generic_page(
                context,
                &format!("Folder: {folder}"),
                "Published notes in this folder.",
                &render_listing_section(
                    folder,
                    "Published notes in this folder.",
                    &[format!("{} note(s)", notes.len())],
                    &list,
                ),
                navigation_tree,
                profile,
                false,
                &folder_page_href(&context.deploy_path, folder),
            ),
        ));
    }
    pages
}

fn render_tag_pages(
    context: &RenderContext,
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
    navigation_tree: &[SiteNavigationNode],
    profile: &ResolvedSiteProfile,
) -> Vec<(String, String)> {
    let mut pages = Vec::new();
    let total_tagged_notes = tag_index.values().map(Vec::len).sum::<usize>();
    let overview = tag_index
        .iter()
        .map(|(tag, notes)| {
            render_card(
                &format!("#{tag}"),
                &tag_page_href(&context.deploy_path, tag),
                &format!("{} published note(s)", notes.len()),
            )
        })
        .collect::<String>();
    pages.push((
        "tags/index.html".to_string(),
        render_generic_page(
            context,
            "Tags",
            "Published tags across the current profile.",
            &render_listing_section(
                "Tag browser",
                "Browse the published subset by tag.",
                &[
                    format!("{} tag(s)", tag_index.len()),
                    format!("{total_tagged_notes} tagged note reference(s)"),
                ],
                &overview,
            ),
            navigation_tree,
            profile,
            false,
            &prefixed_site_path(&context.deploy_path, "/tags/"),
        ),
    ));
    for (tag, notes) in tag_index {
        let list = notes
            .iter()
            .map(|note| render_card(&note.title, &note.route.url_path, &note.excerpt))
            .collect::<String>();
        pages.push((
            format!("tags/{}/index.html", slugify_segment(tag)),
            render_generic_page(
                context,
                &format!("Tag: #{tag}"),
                "Published notes with this tag.",
                &render_listing_section(
                    &format!("#{tag}"),
                    "Published notes with this tag.",
                    &[format!("{} note(s)", notes.len())],
                    &list,
                ),
                navigation_tree,
                profile,
                false,
                &tag_page_href(&context.deploy_path, tag),
            ),
        ));
    }
    pages
}

fn render_generic_page(
    context: &RenderContext,
    title: &str,
    description: &str,
    body: &str,
    navigation_tree: &[SiteNavigationNode],
    profile: &ResolvedSiteProfile,
    search_page: bool,
    canonical_path: &str,
) -> String {
    let canonical_url = canonical_url_for_path(context.base_url.as_deref(), canonical_path);
    let body = format!(
        "<section class=\"site-main\"><h1>{}</h1><p class=\"site-meta\">{}</p>{}</section>",
        escape_html(title),
        escape_html(description),
        body
    );
    render_document_shell(
        context,
        title,
        description,
        &body,
        navigation_tree,
        &[],
        profile,
        search_page,
        canonical_url.as_deref(),
        None,
        None,
    )
}

fn render_document_shell(
    context: &RenderContext,
    title: &str,
    description: &str,
    body: &str,
    navigation_tree: &[SiteNavigationNode],
    modules: &[SiteShellModule],
    profile: &ResolvedSiteProfile,
    _search_page: bool,
    canonical_url: Option<&str>,
    summary_image_url: Option<&str>,
    current_note_path: Option<&str>,
) -> String {
    let document_title = render_page_title(profile, context, title);
    let head_assets = render_head_assets(context, profile);
    let default_nav = render_top_nav(context, profile);
    let explorer = if profile.navigation.explorer {
        render_navigation_tree_html(navigation_tree, current_note_path, &profile.navigation)
    } else {
        String::new()
    };
    let search_button = if profile.search {
        "<button type=\"button\" class=\"site-control-button site-search-launch\" data-site-search-open aria-haspopup=\"dialog\">Search</button>"
    } else {
        ""
    };
    let palette_controls = render_palette_controls(profile);
    let theme_toggle = palette_controls.as_str();
    let reader_mode_toggle = render_reader_mode_toggle(profile);
    let search_dialog = if profile.search {
        concat!(
            "<div class=\"site-search-dialog\" data-site-search-dialog hidden>",
            "<div class=\"site-search-dialog-panel\" role=\"dialog\" aria-modal=\"true\" aria-labelledby=\"site-search-dialog-title\">",
            "<div class=\"site-search-dialog-header\"><h2 id=\"site-search-dialog-title\">Search published notes</h2><button type=\"button\" data-site-search-close aria-label=\"Close search\">Close</button></div>",
            "<p id=\"site-search-dialog-hint\" class=\"site-meta\">Press / to open search from anywhere, then use Esc to close it.</p>",
            "<label class=\"site-visually-hidden\" for=\"site-search-dialog-input\">Search published notes</label>",
            "<input id=\"site-search-dialog-input\" class=\"site-search-input\" data-site-search-input type=\"search\" inputmode=\"search\" enterkeyhint=\"search\" autocomplete=\"off\" spellcheck=\"false\" aria-describedby=\"site-search-dialog-hint\" aria-keyshortcuts=\"/\" placeholder=\"Search titles, excerpts, and tags…\" />",
            "<ol class=\"site-search-results\" data-site-search-results aria-live=\"polite\"></ol>",
            "</div></div>"
        )
        .to_string()
    } else {
        String::new()
    };
    let canonical = canonical_url
        .map(|url| format!("<link rel=\"canonical\" href=\"{}\" />", escape_html(url)))
        .unwrap_or_default();
    let og_url = canonical_url
        .map(|url| {
            format!(
                "<meta property=\"og:url\" content=\"{}\" />",
                escape_html(url)
            )
        })
        .unwrap_or_default();
    let summary_image = summary_image_url.map_or_else(
        || "<meta name=\"twitter:card\" content=\"summary\" />".to_string(),
        |url| {
            format!(
                concat!(
                    "<meta property=\"og:image\" content=\"{}\" />",
                    "<meta name=\"twitter:card\" content=\"summary_large_image\" />",
                    "<meta name=\"twitter:image\" content=\"{}\" />"
                ),
                escape_html(url),
                escape_html(url)
            )
        },
    );
    let rss = if profile.rss && context.base_url.is_some() {
        format!(
            "<link rel=\"alternate\" type=\"application/rss+xml\" href=\"{}\" title=\"RSS\" />",
            escape_html(&prefixed_site_path(&context.deploy_path, "/rss.xml"))
        )
    } else {
        String::new()
    };
    let logo = render_logo(context, profile);
    let default_toolbar = render_default_toolbar(
        search_button,
        &reader_mode_toggle,
        profile.shell.left_rail,
        profile.shell.right_rail && !modules.is_empty(),
    );
    let default_left_rail = if profile.shell.left_rail {
        render_default_left_rail(
            context,
            profile,
            &default_nav,
            search_button,
            &palette_controls,
            &reader_mode_toggle,
            profile.shell.right_rail && !modules.is_empty(),
            &explorer,
            &logo,
        )
    } else {
        String::new()
    };
    let default_right_rail =
        render_default_right_rail(modules, profile.shell.right_rail && !modules.is_empty());
    let default_tokens = render_shell_theme_tokens(
        context,
        profile,
        title,
        description,
        canonical_url,
        current_note_path,
        &default_nav,
        search_button,
        theme_toggle,
        &palette_controls,
        &reader_mode_toggle,
        &explorer,
        &default_toolbar,
        &default_left_rail,
        &default_right_rail,
        &logo,
    );
    let nav = profile.theme_overrides.nav.as_deref().map_or_else(
        || default_nav.clone(),
        |partial| render_theme_partial(Some(partial), &default_tokens),
    );
    let tokens = render_shell_theme_tokens(
        context,
        profile,
        title,
        description,
        canonical_url,
        current_note_path,
        &nav,
        search_button,
        theme_toggle,
        &palette_controls,
        &reader_mode_toggle,
        &explorer,
        &default_toolbar,
        &default_left_rail,
        &default_right_rail,
        &logo,
    );
    let head_partial = render_theme_partial(profile.theme_overrides.head.as_deref(), &tokens);
    let toolbar = profile.theme_overrides.toolbar.as_deref().map_or_else(
        || default_toolbar.clone(),
        |partial| render_theme_partial(Some(partial), &tokens),
    );
    let header = profile
        .theme_overrides
        .header
        .as_deref()
        .map_or_else(String::new, |partial| {
            render_theme_partial(Some(partial), &tokens)
        });
    let left_rail = if profile.shell.left_rail {
        profile.theme_overrides.left_rail.as_deref().map_or_else(
            || default_left_rail.clone(),
            |partial| render_theme_partial(Some(partial), &tokens),
        )
    } else {
        String::new()
    };
    let right_rail = if profile.shell.right_rail {
        profile.theme_overrides.right_rail.as_deref().map_or_else(
            || default_right_rail.clone(),
            |partial| render_theme_partial(Some(partial), &tokens),
        )
    } else {
        String::new()
    };
    let footer = profile
        .theme_overrides
        .footer
        .as_deref()
        .map_or_else(render_default_footer, |partial| {
            render_theme_partial(Some(partial), &tokens)
        });
    let left_rail_enabled = !left_rail.is_empty();
    let right_rail_enabled = !right_rail.is_empty();
    format!(
        concat!(
            "<!doctype html><html lang=\"{}\"><head><meta charset=\"utf-8\" />",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />",
            "<title>{}</title><meta name=\"description\" content=\"{}\" />{}{}{}",
            "<meta property=\"og:title\" content=\"{}\" />",
            "<meta property=\"og:description\" content=\"{}\" />{}",
            "<link rel=\"stylesheet\" href=\"{}\" />{}{}",
            "<script defer src=\"{}\"></script></head>",
            "<body data-search-asset=\"{}\" data-graph-asset=\"{}\" data-live-reload-url=\"{}\" data-live-reload-sse-url=\"{}\" data-current-note-path=\"{}\" data-site-profile=\"{}\" data-site-deploy-path=\"{}\" data-default-palette=\"{}\" data-reader-mode-enabled=\"{}\" data-left-rail-enabled=\"{}\" data-right-rail-enabled=\"{}\"><a class=\"site-skip-link\" href=\"#main-content\">Skip to content</a>{}<div class=\"site-shell\">",
            "{}{}<div class=\"site-layout\"><aside id=\"site-left-rail\" class=\"site-left-rail{}\" aria-label=\"Site navigation\">{}</aside><main id=\"main-content\" class=\"site-content\">{}</main><aside id=\"site-right-rail\" class=\"site-right-rail{}\" aria-label=\"Page context\">{}</aside></div>{}</div></body></html>"
        ),
        escape_html(&context.language),
        escape_html(&document_title),
        escape_html(description),
        canonical,
        rss,
        summary_image,
        escape_html(title),
        escape_html(description),
        og_url,
        escape_html(&prefixed_site_path(&context.deploy_path, "/assets/vulcan-site.css")),
        head_assets,
        head_partial,
        escape_html(&prefixed_site_path(&context.deploy_path, "/assets/vulcan-site.js")),
        if profile.search {
            prefixed_site_path(&context.deploy_path, "/assets/search-index.json")
        } else {
            String::new()
        },
        if profile.graph {
            prefixed_site_path(&context.deploy_path, "/assets/graph.json")
        } else {
            String::new()
        },
        prefixed_site_path(&context.deploy_path, "/__vulcan_site/live-reload.json"),
        prefixed_site_path(&context.deploy_path, "/__vulcan_site/live-reload.events"),
        current_note_path.unwrap_or_default(),
        escape_html(&context.profile),
        escape_html(&context.deploy_path),
        site_palette_mode_name(profile.shell.default_palette),
        if profile.shell.reader_mode { "true" } else { "false" },
        if left_rail_enabled { "true" } else { "false" },
        if right_rail_enabled { "true" } else { "false" },
        search_dialog,
        toolbar,
        header,
        if left_rail_enabled { "" } else { " is-disabled" },
        left_rail,
        body,
        if right_rail_enabled { "" } else { " is-disabled" },
        right_rail,
        footer,
    )
}

fn render_page_title(
    profile: &ResolvedSiteProfile,
    context: &RenderContext,
    title: &str,
) -> String {
    let rendered = profile
        .page_title_template
        .replace("{page}", title)
        .replace("{site}", &context.site_title)
        .replace("{profile}", &profile.name);
    if rendered.trim().is_empty() {
        title.to_string()
    } else {
        rendered
    }
}

fn render_default_toolbar(
    search_button: &str,
    reader_mode_toggle: &str,
    left_rail_enabled: bool,
    right_rail_enabled: bool,
) -> String {
    let left_toggle = if left_rail_enabled {
        "<button class=\"site-control-button\" type=\"button\" data-site-rail-toggle=\"left\" aria-controls=\"site-left-rail\" aria-expanded=\"false\">Browse</button>"
    } else {
        ""
    };
    let right_toggle = if right_rail_enabled {
        "<button class=\"site-control-button\" type=\"button\" data-site-rail-toggle=\"right\" aria-controls=\"site-right-rail\" aria-expanded=\"false\">Panels</button>"
    } else {
        ""
    };
    if left_toggle.is_empty()
        && search_button.is_empty()
        && reader_mode_toggle.is_empty()
        && right_toggle.is_empty()
    {
        return String::new();
    }
    format!(
        "<div class=\"site-mobile-dock\" aria-label=\"Site controls\">{left_toggle}{search_button}{reader_mode_toggle}{right_toggle}</div>",
    )
}

fn render_default_left_rail(
    context: &RenderContext,
    profile: &ResolvedSiteProfile,
    nav: &str,
    search_button: &str,
    palette_controls: &str,
    reader_mode_toggle: &str,
    right_rail_enabled: bool,
    explorer: &str,
    logo: &str,
) -> String {
    let panels_toggle = if right_rail_enabled {
        "<button class=\"site-control-button\" type=\"button\" data-site-rail-toggle=\"right\" aria-controls=\"site-right-rail\" aria-expanded=\"false\">Panels</button>"
    } else {
        ""
    };
    let explorer_section = if explorer.is_empty() {
        String::new()
    } else {
        format!(
            concat!(
                "<section class=\"site-rail-section site-explorer-panel\">",
                "<div class=\"site-rail-section-title\">Browse</div>",
                "<label class=\"site-visually-hidden\" for=\"site-explorer-filter\">Filter navigation</label>",
                "<input id=\"site-explorer-filter\" class=\"site-explorer-filter\" data-site-explorer-filter type=\"search\" autocomplete=\"off\" spellcheck=\"false\" placeholder=\"Filter pages\" />",
                "{}</section>"
            ),
            explorer
        )
    };
    format!(
        concat!(
            "<div class=\"site-rail-shell\">",
            "<div class=\"site-rail-header\">",
            "<div class=\"site-brand-card\">{}<div><p class=\"site-brand-title\"><a href=\"{}\">{}</a></p><p class=\"site-meta\">{}</p></div></div>",
            "<div class=\"site-rail-controls\">{}<div class=\"site-rail-button-row\">{}{}</div>{}</div>",
            "</div>",
            "<nav class=\"site-primary-nav\" aria-label=\"Primary\">{}</nav>",
            "{}",
            "</div>"
        ),
        logo,
        escape_html(&site_root_href(&context.deploy_path)),
        escape_html(&context.site_title),
        escape_html(&format!("{} profile", profile.name)),
        search_button,
        reader_mode_toggle,
        panels_toggle,
        palette_controls,
        nav,
        explorer_section,
    )
}

fn render_default_right_rail(modules: &[SiteShellModule], enabled: bool) -> String {
    if !enabled || modules.is_empty() {
        return String::new();
    }
    let toggles = modules
        .iter()
        .map(|module| {
            format!(
                "<button class=\"site-module-toggle\" type=\"button\" data-site-module-toggle=\"{}\" aria-pressed=\"true\">{}</button>",
                escape_html(module.id),
                escape_html(module.title)
            )
        })
        .collect::<String>();
    let panels = modules
        .iter()
        .map(render_shell_module_html)
        .collect::<String>();
    format!(
        "<div class=\"site-module-toolbar\" role=\"toolbar\" aria-label=\"Page panels\">{toggles}</div>{panels}"
    )
}

fn render_shell_module_html(module: &SiteShellModule) -> String {
    format!(
        concat!(
            "<section class=\"site-panel\" data-site-module=\"{}\">",
            "<button class=\"site-panel-heading\" type=\"button\" data-site-panel-toggle=\"{}\" aria-expanded=\"true\">",
            "<span>{}</span><span class=\"site-panel-chevron\" aria-hidden=\"true\">▾</span>",
            "</button><div class=\"site-panel-body\">{}</div></section>"
        ),
        escape_html(module.id),
        escape_html(module.id),
        escape_html(module.title),
        module.body_html,
    )
}

fn render_palette_controls(profile: &ResolvedSiteProfile) -> String {
    let buttons = [("system", "System"), ("light", "Light"), ("dark", "Dark")]
        .into_iter()
        .map(|(mode, label)| {
            format!(
                "<button class=\"site-palette-button\" type=\"button\" data-theme-mode=\"{}\" aria-pressed=\"{}\">{}</button>",
                mode,
                if mode == site_palette_mode_name(profile.shell.default_palette) {
                    "true"
                } else {
                    "false"
                },
                label
            )
        })
        .collect::<String>();
    format!(
        "<div class=\"site-palette-group\" role=\"group\" aria-label=\"Color theme\">{buttons}</div>"
    )
}

fn render_reader_mode_toggle(profile: &ResolvedSiteProfile) -> String {
    if !profile.shell.reader_mode {
        return String::new();
    }
    "<button class=\"site-control-button\" type=\"button\" data-reader-mode-toggle aria-pressed=\"false\">Read</button>"
        .to_string()
}

fn render_head_assets(context: &RenderContext, profile: &ResolvedSiteProfile) -> String {
    let mut rendered = String::new();
    if let Some(favicon) = profile.favicon.as_ref() {
        write!(
            rendered,
            "<link rel=\"icon\" href=\"{}\" />",
            escape_html(&prefixed_site_path(
                &context.deploy_path,
                &format!(
                    "/assets/{}",
                    encode_url_path(normalize_relative_path(favicon))
                ),
            ))
        )
        .expect("writing to string cannot fail");
    }
    for asset in &profile.extra_css {
        write!(
            rendered,
            "<link rel=\"stylesheet\" href=\"{}\" />",
            escape_html(&prefixed_site_path(
                &context.deploy_path,
                &format!(
                    "/assets/{}",
                    encode_url_path(normalize_relative_path(asset))
                ),
            ))
        )
        .expect("writing to string cannot fail");
    }
    for asset in &profile.extra_js {
        write!(
            rendered,
            "<script defer src=\"{}\"></script>",
            escape_html(&prefixed_site_path(
                &context.deploy_path,
                &format!(
                    "/assets/{}",
                    encode_url_path(normalize_relative_path(asset))
                ),
            ))
        )
        .expect("writing to string cannot fail");
    }
    rendered
}

fn render_logo(context: &RenderContext, profile: &ResolvedSiteProfile) -> String {
    profile.logo.as_ref().map_or_else(String::new, |logo| {
        format!(
            "<img class=\"site-brand-mark\" src=\"{}\" alt=\"\" />",
            escape_html(&prefixed_site_path(
                &context.deploy_path,
                &format!("/assets/{}", encode_url_path(normalize_relative_path(logo))),
            ))
        )
    })
}

fn render_top_nav(context: &RenderContext, profile: &ResolvedSiteProfile) -> String {
    let mut items = Vec::new();
    if profile.navigation.show_home {
        items.push(("Home", site_root_href(&context.deploy_path)));
    }
    if profile.navigation.show_recent {
        items.push((
            "Recent",
            prefixed_site_path(&context.deploy_path, "/recent/"),
        ));
    }
    if profile.navigation.show_folders {
        items.push((
            "Folders",
            prefixed_site_path(&context.deploy_path, "/folders/"),
        ));
    }
    if profile.navigation.show_tags {
        items.push(("Tags", prefixed_site_path(&context.deploy_path, "/tags/")));
    }
    if profile.search {
        items.push((
            "Search",
            prefixed_site_path(&context.deploy_path, "/search/"),
        ));
    }
    if profile.navigation.show_graph && profile.graph {
        items.push(("Graph", prefixed_site_path(&context.deploy_path, "/graph/")));
    }
    items
        .into_iter()
        .map(|(label, href)| {
            format!(
                "<a class=\"site-nav-link\" href=\"{href}\">{}</a>",
                escape_html(label)
            )
        })
        .collect::<String>()
}

fn render_default_footer() -> String {
    "<footer class=\"site-footer\">Built by Vulcan static site builder.</footer>".to_string()
}

fn render_theme_partial(template: Option<&str>, replacements: &[(&str, String)]) -> String {
    let Some(template) = template else {
        return String::new();
    };
    replacements
        .iter()
        .fold(template.to_string(), |rendered, (token, value)| {
            rendered.replace(token, value)
        })
}

fn render_shell_theme_tokens(
    context: &RenderContext,
    profile: &ResolvedSiteProfile,
    title: &str,
    description: &str,
    canonical_url: Option<&str>,
    current_note_path: Option<&str>,
    nav: &str,
    search_button: &str,
    theme_toggle: &str,
    palette_controls: &str,
    reader_mode_toggle: &str,
    explorer: &str,
    toolbar: &str,
    left_rail: &str,
    right_rail: &str,
    logo: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("{{site_title}}", escape_html(&context.site_title)),
        ("{{page_title}}", escape_html(title)),
        ("{{page_description}}", escape_html(description)),
        ("{{profile_name}}", escape_html(&profile.name)),
        (
            "{{home_href}}",
            escape_html(&site_root_href(&context.deploy_path)),
        ),
        ("{{deploy_path}}", escape_html(&context.deploy_path)),
        (
            "{{canonical_url}}",
            canonical_url.map_or_else(String::new, escape_html),
        ),
        (
            "{{current_note_path}}",
            current_note_path.map_or_else(String::new, escape_html),
        ),
        ("{{nav}}", nav.to_string()),
        ("{{search_button}}", search_button.to_string()),
        ("{{theme_toggle}}", theme_toggle.to_string()),
        ("{{palette_controls}}", palette_controls.to_string()),
        ("{{reader_mode_toggle}}", reader_mode_toggle.to_string()),
        ("{{explorer}}", explorer.to_string()),
        ("{{toolbar}}", toolbar.to_string()),
        ("{{left_rail}}", left_rail.to_string()),
        ("{{right_rail}}", right_rail.to_string()),
        ("{{site_logo}}", logo.to_string()),
    ]
}

fn render_note_theme_chrome(
    context: &RenderContext,
    profile: &ResolvedSiteProfile,
    title: &str,
    description: &str,
    canonical_url: Option<&str>,
    note_path: &str,
    body_html: &str,
) -> String {
    let tokens = render_shell_theme_tokens(
        context,
        profile,
        title,
        description,
        canonical_url,
        Some(note_path),
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
    );
    format!(
        "{}{}{}",
        render_theme_partial(profile.theme_overrides.note_before.as_deref(), &tokens),
        body_html,
        render_theme_partial(profile.theme_overrides.note_after.as_deref(), &tokens),
    )
}

fn render_listing_cards(notes: &[RenderedNote]) -> String {
    let cards = notes
        .iter()
        .map(|note| render_card(&note.title, &note.route.url_path, &note.excerpt))
        .collect::<String>();
    render_listing_section(
        "Published notes",
        "Browse the notes available in this site profile.",
        &[format!("{} note(s)", notes.len())],
        &cards,
    )
}

fn render_listing_section(title: &str, description: &str, stats: &[String], cards: &str) -> String {
    let stats = stats
        .iter()
        .map(|stat| {
            format!(
                "<span class=\"site-listing-stat\">{}</span>",
                escape_html(stat)
            )
        })
        .collect::<String>();
    format!(
        concat!(
            "<section class=\"site-listing\">",
            "<header class=\"site-listing-hero\">",
            "<p class=\"site-listing-kicker\">Index</p>",
            "<h2>{}</h2>",
            "<p>{}</p>",
            "<div class=\"site-listing-meta\">{}</div>",
            "</header>",
            "<div class=\"site-card-grid\">{}</div>",
            "</section>"
        ),
        escape_html(title),
        escape_html(description),
        stats,
        cards
    )
}

fn render_card(title: &str, href: &str, excerpt: &str) -> String {
    format!(
        "<article class=\"site-card\"><h3><a href=\"{}\">{}</a></h3><p>{}</p></article>",
        escape_html(href),
        escape_html(title),
        escape_html(excerpt)
    )
}

fn render_breadcrumbs(breadcrumbs: &[String]) -> String {
    if breadcrumbs.is_empty() {
        return String::new();
    }
    let parts = breadcrumbs
        .iter()
        .map(|crumb| format!("<span>{}</span>", escape_html(crumb)))
        .collect::<Vec<_>>()
        .join("<span>/</span>");
    format!("<nav class=\"site-breadcrumbs\" aria-label=\"Breadcrumbs\">{parts}</nav>")
}

fn render_prev_next(previous: Option<&RenderedNote>, next: Option<&RenderedNote>) -> String {
    if previous.is_none() && next.is_none() {
        return String::new();
    }
    let previous_link = previous.map_or_else(String::new, |note| {
        format!(
            "<a href=\"{}\">← {}</a>",
            note.route.url_path,
            escape_html(&note.title)
        )
    });
    let next_link = next.map_or_else(String::new, |note| {
        format!(
            "<a href=\"{}\">{} →</a>",
            note.route.url_path,
            escape_html(&note.title)
        )
    });
    format!(
        "<nav class=\"site-inline-nav\" aria-label=\"Pagination\">{previous_link}{next_link}</nav>"
    )
}

fn render_note_tags(deploy_path: &str, tags: &[String]) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let items = tags
        .iter()
        .map(|tag| {
            let normalized = normalize_tag(tag);
            format!(
                "<li><a href=\"{}\">#{}</a></li>",
                tag_page_href(deploy_path, &normalized),
                escape_html(&normalized)
            )
        })
        .collect::<String>();
    format!("<ul class=\"site-pill-list\">{items}</ul>")
}

fn render_note_diagnostics(diagnostics: &[HtmlRenderDiagnostic]) -> String {
    if diagnostics.is_empty() {
        return String::new();
    }
    let items = diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "<li>{}: {}</li>",
                escape_html(&diagnostic.kind),
                escape_html(&diagnostic.message)
            )
        })
        .collect::<String>();
    format!(
        "<section class=\"site-diagnostics\"><h2>Render diagnostics</h2><ul>{items}</ul></section>"
    )
}

fn render_folder_summary(
    deploy_path: &str,
    note: &RenderedNote,
    folder_index: &BTreeMap<String, Vec<&RenderedNote>>,
) -> String {
    let folder = folder_for_note(&note.source_path);
    if folder.is_empty() {
        return String::new();
    }
    let count = folder_index.get(&folder).map_or(0, Vec::len);
    format!(
        "<section class=\"site-callout\"><a href=\"{}\">Folder view</a> includes {} note(s).</section>",
        folder_page_href(deploy_path, &folder),
        count
    )
}

fn render_related_tags(
    note: &RenderedNote,
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
) -> String {
    let related = collect_related_note_entries(note, tag_index);
    if related.is_empty() {
        return String::new();
    }
    let items = related
        .into_iter()
        .take(6)
        .map(|entry| {
            format!(
                "<li><a href=\"{}\">{}</a></li>",
                escape_html(&entry.url),
                escape_html(&entry.source_path)
            )
        })
        .collect::<String>();
    format!(
        "<section class=\"site-callout\"><strong>Related notes</strong><ul>{items}</ul></section>"
    )
}

fn build_hover_manifest(notes: &[RenderedNote]) -> BTreeMap<String, Value> {
    notes
        .iter()
        .map(|note| {
            (
                note.route.url_path.clone(),
                serde_json::json!({
                    "title": note.title,
                    "excerpt": note.excerpt,
                    "url": note.route.url_path,
                    "headings": note.headings,
                }),
            )
        })
        .collect()
}

fn build_recent_manifest(notes: &[RenderedNote]) -> Vec<RecentNoteManifestEntry> {
    let mut notes = notes.iter().collect::<Vec<_>>();
    notes.sort_by(|left, right| {
        right
            .file_mtime
            .cmp(&left.file_mtime)
            .then(left.title.cmp(&right.title))
    });
    notes
        .into_iter()
        .map(|note| RecentNoteManifestEntry {
            source_path: note.source_path.clone(),
            title: note.title.clone(),
            url: note.route.url_path.clone(),
            excerpt: note.excerpt.clone(),
            file_mtime: note.file_mtime,
            tags: note.tags.clone(),
        })
        .collect()
}

fn collect_related_note_entries(
    note: &RenderedNote,
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
) -> Vec<RelatedNoteManifestEntry> {
    let mut related = BTreeMap::<String, (String, String, String, BTreeSet<String>)>::new();
    for tag in &note.tags {
        let normalized = normalize_tag(tag);
        if let Some(notes) = tag_index.get(&normalized) {
            for related_note in notes {
                if related_note.source_path == note.source_path {
                    continue;
                }
                let entry = related
                    .entry(related_note.source_path.clone())
                    .or_insert_with(|| {
                        (
                            related_note.title.clone(),
                            related_note.route.url_path.clone(),
                            related_note.excerpt.clone(),
                            BTreeSet::new(),
                        )
                    });
                entry.3.insert(normalized.clone());
            }
        }
    }
    let mut entries = related
        .into_iter()
        .map(
            |(source_path, (title, url, excerpt, shared_tags))| RelatedNoteManifestEntry {
                source_path,
                title,
                url,
                excerpt,
                shared_tags: shared_tags.into_iter().collect(),
            },
        )
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .shared_tags
            .len()
            .cmp(&left.shared_tags.len())
            .then(left.title.cmp(&right.title))
            .then(left.source_path.cmp(&right.source_path))
    });
    entries
}

fn build_related_manifest(
    notes: &[RenderedNote],
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
) -> BTreeMap<String, Vec<RelatedNoteManifestEntry>> {
    notes
        .iter()
        .map(|note| {
            (
                note.route.url_path.clone(),
                collect_related_note_entries(note, tag_index),
            )
        })
        .collect()
}

fn build_search_index(
    rendered_notes: &[RenderedNote],
    search_text_by_path: &HashMap<String, String>,
) -> Value {
    let mut documents = Vec::with_capacity(rendered_notes.len());
    let mut terms = BTreeMap::<String, BTreeMap<usize, u16>>::new();
    let mut total_length = 0usize;

    for (id, note) in rendered_notes.iter().enumerate() {
        // Reuse source-derived search text from the render pipeline so site builds do not need to
        // strip or tokenize rendered HTML again just to produce the client search asset.
        let search_text = search_text_by_path
            .get(&note.source_path)
            .filter(|value| !value.is_empty())
            .map_or(note.excerpt.as_str(), String::as_str);
        let preview = if note.excerpt.trim().is_empty() {
            search_text.chars().take(240).collect::<String>()
        } else {
            note.excerpt.clone()
        };

        let mut frequencies = BTreeMap::<String, u16>::new();
        accumulate_search_terms(&mut frequencies, &note.title, 4);
        accumulate_search_terms(&mut frequencies, &note.excerpt, 2);
        accumulate_search_terms(&mut frequencies, &note.aliases.join(" "), 3);
        let length = accumulate_search_terms(&mut frequencies, search_text, 1).max(1);
        total_length += length;
        for tag in &note.tags {
            accumulate_search_terms(&mut frequencies, tag, 3);
        }

        for (term, tf) in frequencies {
            terms.entry(term).or_default().insert(id, tf);
        }

        documents.push(SearchIndexDocument {
            id,
            title: note.title.clone(),
            url: note.route.url_path.clone(),
            excerpt: note.excerpt.clone(),
            preview,
            tags: note.tags.clone(),
            length,
        });
    }

    let average_length = if documents.is_empty() {
        1.0
    } else {
        f64::from(u32::try_from(total_length).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(documents.len()).unwrap_or(u32::MAX))
    };
    let serialized_terms = terms
        .into_iter()
        .map(|(term, postings)| {
            let entries = postings
                .into_iter()
                .map(|(id, tf)| SearchIndexPosting { id, tf })
                .collect::<Vec<_>>();
            (term, entries)
        })
        .collect::<BTreeMap<_, _>>();

    serde_json::json!({
        "version": 2,
        "average_length": average_length,
        "documents": documents,
        "terms": serialized_terms,
    })
}

fn build_graph_asset(
    paths: &VaultPaths,
    rendered_notes: &[RenderedNote],
) -> Result<String, AppError> {
    let published = rendered_notes
        .iter()
        .map(|note| note.source_path.as_str())
        .collect::<HashSet<_>>();
    let route_map = rendered_notes
        .iter()
        .map(|note| (note.source_path.as_str(), note.route.url_path.clone()))
        .collect::<HashMap<_, _>>();
    let title_map = rendered_notes
        .iter()
        .map(|note| (note.source_path.as_str(), note.title.clone()))
        .collect::<HashMap<_, _>>();
    let graph = export_graph(paths).map_err(AppError::operation)?;
    let nodes = graph
        .nodes
        .into_iter()
        .filter(|node| published.contains(node.path.as_str()))
        .map(|node| {
            serde_json::json!({
                "id": node.id,
                "path": node.path,
                "title": title_map.get(node.path.as_str()).cloned().unwrap_or_else(|| trim_markdown_extension(&node.path).to_string()),
                "url": route_map.get(node.path.as_str()).cloned().unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let edges = graph
        .edges
        .into_iter()
        .filter(|edge| {
            published.contains(edge.source.as_str()) && published.contains(edge.target.as_str())
        })
        .map(|edge| serde_json::json!({ "source": edge.source, "target": edge.target }))
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&serde_json::json!({ "nodes": nodes, "edges": edges }))
        .map_err(AppError::operation)
}

fn build_rss_document(context: &RenderContext, notes: &[RenderedNote]) -> String {
    let items = notes
        .iter()
        .take(20)
        .map(|note| {
            let link = context.base_url.as_deref().map_or_else(
                || note.route.url_path.clone(),
                |base| format!("{}{}", base.trim_end_matches('/'), note.route.url_path),
            );
            format!(
                "<item><title>{}</title><link>{}</link><description>{}</description></item>",
                escape_xml(&note.title),
                escape_xml(&link),
                escape_xml(&note.excerpt)
            )
        })
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><rss version=\"2.0\"><channel><title>{}</title><link>{}</link><description>{}</description>{items}</channel></rss>",
        escape_xml(&context.site_title),
        escape_xml(
            &context
                .base_url
                .as_deref()
                .map_or_else(|| site_root_href(&context.deploy_path), |base| {
                    format!(
                        "{}{}",
                        base.trim_end_matches('/'),
                        site_root_href(&context.deploy_path)
                    )
                }),
        ),
        escape_xml(&context.site_title),
    )
}

fn build_sitemap(
    base_url: &str,
    deploy_path: &str,
    files: &BTreeSet<String>,
    notes: &[RenderedNote],
) -> String {
    let note_urls = notes
        .iter()
        .map(|note| note.route.url_path.clone())
        .chain(files.iter().filter_map(|path| {
            if path.ends_with("index.html") {
                Some(prefixed_site_path(
                    deploy_path,
                    path.trim_end_matches("index.html"),
                ))
            } else if Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
            {
                Some(prefixed_site_path(deploy_path, path))
            } else {
                None
            }
        }))
        .collect::<BTreeSet<_>>();
    let body = note_urls
        .into_iter()
        .map(|path| {
            format!(
                "<url><loc>{}</loc></url>",
                escape_xml(&format!("{}{}", base_url.trim_end_matches('/'), path))
            )
        })
        .collect::<String>();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{body}</urlset>")
}

fn links_by_source(links: &[ExportLinkRecord]) -> HashMap<&str, Vec<&ExportLinkRecord>> {
    let mut grouped = HashMap::<&str, Vec<&ExportLinkRecord>>::new();
    for link in links {
        grouped
            .entry(link.source_document_path.as_str())
            .or_default()
            .push(link);
    }
    grouped
}

fn backlinks_by_target(
    links: &[ExportLinkRecord],
    published_paths: &HashSet<String>,
) -> HashMap<String, Vec<String>> {
    let mut backlinks = HashMap::<String, BTreeSet<String>>::new();
    for link in links {
        let Some(target) = link.resolved_target_path.as_ref() else {
            continue;
        };
        if !published_paths.contains(target)
            || !published_paths.contains(&link.source_document_path)
        {
            continue;
        }
        if !link
            .resolved_target_extension
            .as_deref()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        backlinks
            .entry(target.clone())
            .or_default()
            .insert(link.source_document_path.clone());
    }
    backlinks
        .into_iter()
        .map(|(path, sources)| (path, sources.into_iter().collect::<Vec<_>>()))
        .collect()
}

fn build_tag_href_map(notes: &[SitePlanNote], deploy_path: &str) -> HashMap<String, String> {
    notes
        .iter()
        .flat_map(|note| note.note.tags.iter())
        .map(|tag| normalize_tag(tag))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|tag| (tag.clone(), tag_page_href(deploy_path, &tag)))
        .collect()
}

fn build_tag_index(notes: &[RenderedNote]) -> BTreeMap<String, Vec<&RenderedNote>> {
    let mut index = BTreeMap::<String, Vec<&RenderedNote>>::new();
    for note in notes {
        for tag in &note.tags {
            index.entry(normalize_tag(tag)).or_default().push(note);
        }
    }
    index
}

fn build_folder_index(notes: &[RenderedNote]) -> BTreeMap<String, Vec<&RenderedNote>> {
    let mut index = BTreeMap::<String, Vec<&RenderedNote>>::new();
    for note in notes {
        index
            .entry(folder_for_note(&note.source_path))
            .or_default()
            .push(note);
    }
    index.retain(|_, notes| !notes.is_empty());
    index
}

fn resolve_home_note<'a>(
    profile: &ResolvedSiteProfile,
    notes: &'a [RenderedNote],
) -> Option<&'a RenderedNote> {
    profile.home.as_deref().and_then(|home| {
        notes
            .iter()
            .find(|note| note.source_path == home || note.source_path == format!("{home}.md"))
            .or_else(|| {
                notes
                    .iter()
                    .find(|note| trim_markdown_extension(&note.source_path).ends_with(home))
            })
    })
}

fn collect_asset_links(links: &[ExportLinkRecord], deploy_path: &str) -> HashMap<String, String> {
    links
        .iter()
        .filter_map(|link| {
            if !is_markdown_asset(link) {
                return None;
            }
            let target = link.resolved_target_path.as_ref()?;
            Some((
                target.clone(),
                prefixed_site_path(deploy_path, &format!("/assets/{}", encode_url_path(target))),
            ))
        })
        .collect()
}

fn apply_link_policy_to_source<'a>(
    source: &'a str,
    links: &[&ExportLinkRecord],
    published_paths: &HashSet<String>,
    policy: SiteLinkPolicyConfig,
) -> Cow<'a, str> {
    if !matches!(
        policy,
        SiteLinkPolicyConfig::DropLink | SiteLinkPolicyConfig::RenderPlainText
    ) {
        return Cow::Borrowed(source);
    }
    let mut replacements = links
        .iter()
        .filter_map(|link| {
            let unresolved_note = link
                .resolved_target_extension
                .as_deref()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                && link
                    .resolved_target_path
                    .as_ref()
                    .is_none_or(|path| !published_paths.contains(path));
            if !unresolved_note {
                return None;
            }
            let start = usize::try_from(link.byte_offset).ok()?;
            let end = start.checked_add(link.raw_text.len())?;
            let replacement = if link.link_kind.eq_ignore_ascii_case("embed")
                && policy == SiteLinkPolicyConfig::DropLink
            {
                String::new()
            } else {
                link.display_text
                    .clone()
                    .or_else(|| link.target_path_candidate.clone())
                    .unwrap_or_default()
            };
            Some((start, end, replacement))
        })
        .collect::<Vec<_>>();
    if replacements.is_empty() {
        return Cow::Borrowed(source);
    }
    replacements.sort_by_key(|(start, _, _)| *start);
    replacements.reverse();
    let mut rendered = source.to_string();
    for (start, end, replacement) in replacements {
        if start <= end && end <= rendered.len() {
            rendered.replace_range(start..end, &replacement);
        }
    }
    Cow::Owned(rendered)
}

fn is_markdown_asset(link: &ExportLinkRecord) -> bool {
    is_internal_asset_link(link)
        && link
            .resolved_target_extension
            .as_deref()
            .is_some_and(|extension| !extension.eq_ignore_ascii_case("md"))
}

fn is_internal_asset_link(link: &ExportLinkRecord) -> bool {
    !link.link_kind.eq_ignore_ascii_case("external")
        && link.target_path_candidate.is_some()
        && !link
            .resolved_target_extension
            .as_deref()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn note_route_slug(note: &NoteRecord, profile_name: &str) -> String {
    frontmatter_override(note, profile_name, "slug")
        .or_else(|| folder_note_path(&note.document_path))
        .unwrap_or_else(|| trim_markdown_extension(&note.document_path).to_string())
}

fn note_title(note: &NoteRecord, profile_name: &str, rendered_title: Option<&str>) -> String {
    frontmatter_override(note, profile_name, "title")
        .or_else(|| rendered_title.map(ToOwned::to_owned))
        .unwrap_or_else(|| note.file_name.clone())
}

fn canonical_url_for_path(base_url: Option<&str>, url_path: &str) -> Option<String> {
    base_url.map(|base| {
        format!(
            "{}{}",
            base.trim_end_matches('/'),
            if url_path.starts_with('/') {
                url_path.to_string()
            } else {
                format!("/{url_path}")
            }
        )
    })
}

fn normalize_site_metadata_url(base_url: Option<&str>, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        return Some(value.to_string());
    }
    if value.starts_with('/') {
        return base_url
            .map(|base| format!("{}{}", base.trim_end_matches('/'), value))
            .or_else(|| Some(value.to_string()));
    }
    Some(value.to_string())
}

fn summary_image_meta_url(deploy_path: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with("http://") || value.starts_with("https://") || value.starts_with('/') {
        return Some(value.to_string());
    }
    let relative = normalize_relative_path(value);
    if relative.as_os_str().is_empty() {
        None
    } else {
        Some(prefixed_site_path(
            deploy_path,
            &format!("/assets/{}", encode_url_path(&relative)),
        ))
    }
}

fn summary_image_source_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with('/')
    {
        return None;
    }
    let relative = normalize_relative_path(value);
    (!relative.as_os_str().is_empty()).then_some(relative)
}

fn frontmatter_override(note: &NoteRecord, profile_name: &str, key: &str) -> Option<String> {
    let object = note.frontmatter.as_object()?;
    object
        .get("site")
        .and_then(Value::as_object)
        .and_then(|site| {
            site.get("profiles")
                .and_then(Value::as_object)
                .and_then(|profiles| profiles.get(profile_name))
                .and_then(Value::as_object)
                .and_then(|profile| profile.get(key))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| site.get(key).and_then(Value::as_str).map(ToOwned::to_owned))
        })
        .or_else(|| {
            object
                .get(key)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn frontmatter_string_list(note: &NoteRecord, profile_name: &str, key: &str) -> Vec<String> {
    let Some(object) = note.frontmatter.as_object() else {
        return Vec::new();
    };
    let profile_value = object
        .get("site")
        .and_then(Value::as_object)
        .and_then(|site| {
            site.get("profiles")
                .and_then(Value::as_object)
                .and_then(|profiles| profiles.get(profile_name))
                .and_then(Value::as_object)
                .and_then(|profile| profile.get(key))
                .or_else(|| site.get(key))
        })
        .or_else(|| object.get(key));
    match profile_value {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        Some(Value::String(value)) => vec![value.clone()],
        _ => Vec::new(),
    }
}

fn path_matches_selector(path: &str, selector: &str) -> bool {
    let path = normalize_path(path);
    let selector = normalize_path(selector);
    if selector.contains('*') || selector.contains('?') {
        let regex = glob_selector_regex(&selector);
        regex.is_match(&path)
    } else if selector.ends_with('/') {
        path.starts_with(&selector)
    } else {
        path == selector || path.starts_with(&format!("{selector}/"))
    }
}

fn glob_selector_regex(selector: &str) -> Regex {
    let mut pattern = String::from("^");
    let mut chars = selector.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                pattern.push_str(".*");
            }
            '*' => pattern.push_str("[^/]*"),
            '?' => pattern.push_str("[^/]"),
            other => pattern.push_str(&regex::escape(&other.to_string())),
        }
    }
    pattern.push('$');
    Regex::new(&pattern).expect("generated glob regex should compile")
}

fn breadcrumbs_for_path(path: &str) -> Vec<String> {
    let path = normalize_path(path);
    let mut breadcrumbs = path.split('/').map(ToOwned::to_owned).collect::<Vec<_>>();
    let _ = breadcrumbs.pop();
    breadcrumbs
}

fn folder_for_note(path: &str) -> String {
    let path = normalize_path(path);
    path.rsplit_once('/')
        .map_or_else(String::new, |(folder, _)| folder.to_string())
}

fn folder_note_path(path: &str) -> Option<String> {
    let normalized = normalize_path(path);
    let (folder, file_name) = normalized.rsplit_once('/')?;
    if folder.is_empty() {
        return None;
    }
    if file_name.eq_ignore_ascii_case("index.md") {
        return Some(folder.to_string());
    }
    let folder_name = folder.rsplit('/').next().unwrap_or(folder);
    trim_markdown_extension(file_name)
        .eq_ignore_ascii_case(folder_name)
        .then_some(folder.to_string())
}

fn note_route_aliases(path: &str) -> Vec<String> {
    let normalized = normalize_path(path);
    let mut aliases = Vec::new();
    let mut push_alias = |candidate: String| {
        if !candidate.is_empty() && !aliases.contains(&candidate) {
            aliases.push(candidate);
        }
    };
    push_alias(normalized.clone());
    if let Some(stem) = normalized.strip_suffix(".md") {
        push_alias(stem.to_string());
    }
    if let Some(folder_note) = folder_note_path(&normalized) {
        push_alias(folder_note);
    }
    aliases
}

fn folder_page_href(deploy_path: &str, folder: &str) -> String {
    if folder.is_empty() {
        prefixed_site_path(deploy_path, "/folders/")
    } else {
        prefixed_site_path(deploy_path, &format!("/folders/{}/", slugify_path(folder)))
    }
}

fn tag_page_href(deploy_path: &str, tag: &str) -> String {
    prefixed_site_path(deploy_path, &format!("/tags/{}/", slugify_segment(tag)))
}

fn asset_output_path(output_dir: &Path, asset_path: &str) -> PathBuf {
    output_dir
        .join("assets")
        .join(normalize_relative_path(asset_path))
}

fn collect_site_asset_copy_work_items(
    paths: &VaultPaths,
    plan: &SitePlan,
    rendered_notes: &[RenderedNote],
    output_dir: &Path,
) -> Result<Vec<SiteAssetCopyWorkItem>, AppError> {
    let mut assets = BTreeMap::<String, SiteAssetCopyWorkItem>::new();

    for source_path in collect_asset_links(&plan.links, &plan.profile.deploy_path).keys() {
        insert_site_asset_copy_work_item(&mut assets, output_dir, Path::new(source_path));
    }

    let summary_image_assets = rendered_notes
        .iter()
        .filter_map(|note| note.summary_image.as_deref())
        .filter_map(summary_image_source_path)
        .collect::<BTreeSet<_>>();
    for summary_image in &summary_image_assets {
        insert_site_asset_copy_work_item(&mut assets, output_dir, summary_image);
    }

    for extra_asset in plan
        .profile
        .extra_css
        .iter()
        .chain(plan.profile.extra_js.iter())
        .chain(plan.profile.favicon.iter())
        .chain(plan.profile.logo.iter())
    {
        insert_site_asset_copy_work_item(&mut assets, output_dir, extra_asset);
    }

    for extra_pattern in &plan.profile.asset_policy.include_folders {
        for asset in collect_extra_assets(paths, extra_pattern)? {
            insert_site_asset_copy_work_item(&mut assets, output_dir, &asset);
        }
    }

    Ok(assets.into_values().collect())
}

fn insert_site_asset_copy_work_item(
    assets: &mut BTreeMap<String, SiteAssetCopyWorkItem>,
    output_dir: &Path,
    relative: &Path,
) {
    let source_key = display_path(relative);
    let destination = asset_output_path(output_dir, &source_key);
    let relative_output_path =
        display_path(destination.strip_prefix(output_dir).unwrap_or(&destination));
    assets
        .entry(relative_output_path.clone())
        .or_insert_with(|| SiteAssetCopyWorkItem {
            source_path: relative.to_path_buf(),
            source_key,
            relative_output_path,
            destination,
        });
}

fn collect_extra_assets(paths: &VaultPaths, pattern: &str) -> Result<Vec<PathBuf>, AppError> {
    let mut assets = Vec::new();
    collect_extra_assets_recursive(paths.vault_root(), paths.vault_root(), pattern, &mut assets)?;
    Ok(assets)
}

fn collect_extra_assets_recursive(
    root: &Path,
    directory: &Path,
    pattern: &str,
    assets: &mut Vec<PathBuf>,
) -> Result<(), AppError> {
    for entry in fs::read_dir(directory).map_err(AppError::operation)? {
        let entry = entry.map_err(AppError::operation)?;
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == ".vulcan") {
            continue;
        }
        if entry.file_type().map_err(AppError::operation)?.is_dir() {
            collect_extra_assets_recursive(root, &path, pattern, assets)?;
            continue;
        }
        let relative = path.strip_prefix(root).map_err(AppError::operation)?;
        let relative_display = display_path(relative);
        let is_markdown = Path::new(&relative_display)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
        if !is_markdown && path_matches_selector(&relative_display, pattern) {
            assets.push(relative.to_path_buf());
        }
    }
    Ok(())
}

fn copy_file_from_vault_with_cache(
    paths: &VaultPaths,
    relative: &Path,
    destination: &Path,
    cached: Option<&SiteCopiedAssetState>,
) -> Result<SiteAssetCopyResult, AppError> {
    let source = paths.vault_root().join(relative);
    let source_signature = site_input_signature_for_path(&source)?.ok_or_else(|| {
        AppError::operation(format!(
            "site asset source does not exist: {}",
            source.display()
        ))
    })?;
    let destination_signature = site_input_signature_for_path(destination)?;

    if let (Some(cached), Some(current_output_signature)) = (cached, destination_signature.as_ref())
    {
        if cached.source_signature == source_signature
            && cached.output_signature == *current_output_signature
        {
            let contents = fs::read(&source).map_err(AppError::operation)?;
            if fs::read(destination).ok().as_deref() != Some(contents.as_slice()) {
                let changed = write_output_bytes_if_changed(destination, &contents)?;
                let output_signature =
                    site_input_signature_for_path(destination)?.ok_or_else(|| {
                        AppError::operation(format!(
                            "copied site asset disappeared before it could be tracked: {}",
                            destination.display()
                        ))
                    })?;
                return Ok(SiteAssetCopyResult {
                    changed,
                    state: SiteCopiedAssetState {
                        source_path: display_path(relative),
                        source_signature,
                        output_signature,
                    },
                });
            }
            return Ok(SiteAssetCopyResult {
                changed: false,
                state: SiteCopiedAssetState {
                    source_path: display_path(relative),
                    source_signature,
                    output_signature: current_output_signature.clone(),
                },
            });
        }
    }

    let contents = fs::read(&source).map_err(AppError::operation)?;
    let changed = write_output_bytes_if_changed(destination, &contents)?;
    let output_signature = site_input_signature_for_path(destination)?.ok_or_else(|| {
        AppError::operation(format!(
            "copied site asset disappeared before it could be tracked: {}",
            destination.display()
        ))
    })?;

    Ok(SiteAssetCopyResult {
        changed,
        state: SiteCopiedAssetState {
            source_path: display_path(relative),
            source_signature,
            output_signature,
        },
    })
}

fn site_asset_copy_state_by_source(
    state: &SiteAssetCopyState,
) -> HashMap<&str, &SiteCopiedAssetState> {
    state
        .assets
        .iter()
        .map(|asset| (asset.source_path.as_str(), asset))
        .collect()
}

fn write_output_file(path: &Path, contents: &str) -> Result<bool, AppError> {
    write_output_bytes_if_changed(path, contents.as_bytes())
}

fn render_json_payload<T: Serialize + ?Sized>(value: &T, pretty: bool) -> Result<String, AppError> {
    if pretty {
        serde_json::to_string_pretty(value).map_err(AppError::operation)
    } else {
        serde_json::to_string(value).map_err(AppError::operation)
    }
}

fn write_serialized_json_output<T: Serialize + ?Sized>(
    output_dir: &Path,
    relative_path: &str,
    value: &T,
    pretty: bool,
    dry_run: bool,
    files: &mut BTreeSet<String>,
    changed_files: &mut BTreeSet<String>,
) -> Result<(), AppError> {
    let payload = render_json_payload(value, pretty)?;
    if !dry_run && write_output_file(&output_dir.join(relative_path), &payload)? {
        changed_files.insert(relative_path.to_string());
    }
    files.insert(relative_path.to_string());
    Ok(())
}

fn frontend_bundle_contract_name() -> &'static str {
    "vulcan_frontend_bundle"
}

fn frontend_bundle_root_contract_path() -> &'static str {
    "frontend-bundle.json"
}

fn frontend_bundle_note_index_path() -> &'static str {
    "assets/note-index.json"
}

fn frontend_bundle_navigation_tree_path() -> &'static str {
    "assets/navigation-tree.json"
}

fn frontend_bundle_invalidation_path() -> &'static str {
    "assets/invalidation.json"
}

fn frontend_bundle_schema_path() -> &'static str {
    "schema/frontend-bundle.schema.json"
}

fn frontend_bundle_types_path() -> &'static str {
    "schema/frontend-bundle.d.ts"
}

fn frontend_bundle_note_document_path(route: &SiteRoute) -> String {
    route.output_path.strip_suffix(".html").map_or_else(
        || format!("{}.json", route.output_path),
        |path| format!("{path}.json"),
    )
}

fn is_frontend_bundle_note_document(path: &str) -> bool {
    path.starts_with("notes/")
        && Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

fn frontend_bundle_route_from_document_path(path: &str, deploy_path: &str) -> Option<String> {
    let stripped = path.strip_prefix("notes/")?;
    let route_suffix = stripped.strip_suffix("index.json")?;
    let route_path = if route_suffix.is_empty() {
        "/notes/".to_string()
    } else {
        format!("/notes/{route_suffix}")
    };
    Some(prefixed_site_path(deploy_path, &route_path))
}

fn frontend_bundle_contract_schema_json() -> String {
    r##"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://schemas.vulcan.dev/frontend-bundle/v1.json",
  "title": "Vulcan Frontend Bundle v1",
  "type": "object",
  "required": ["contract", "profile", "context", "note_count", "diagnostics", "routes", "notes", "artifacts"],
  "additionalProperties": false,
  "properties": {
    "contract": { "$ref": "#/$defs/contractInfo" },
    "profile": { "$ref": "#/$defs/profile" },
    "context": { "$ref": "#/$defs/renderContext" },
    "note_count": { "type": "integer", "minimum": 0 },
    "diagnostics": { "type": "array", "items": { "$ref": "#/$defs/siteDiagnostic" } },
    "routes": { "type": "array", "items": { "$ref": "#/$defs/siteRoute" } },
    "notes": { "type": "array", "items": { "$ref": "#/$defs/noteIndexEntry" } },
    "artifacts": { "$ref": "#/$defs/artifactPaths" }
  },
  "$defs": {
    "contractInfo": {
      "type": "object",
      "required": ["name", "version"],
      "additionalProperties": false,
      "properties": {
        "name": { "type": "string" },
        "version": { "type": "integer", "minimum": 1 }
      }
    },
    "profile": {
      "type": "object",
      "required": ["name", "title", "deploy_path", "language", "theme", "search", "graph", "backlinks", "rss", "shell", "navigation", "modules"],
      "additionalProperties": false,
      "properties": {
        "name": { "type": "string" },
        "title": { "type": "string" },
        "deploy_path": { "type": "string" },
        "base_url": { "type": ["string", "null"] },
        "language": { "type": "string" },
        "theme": { "type": "string" },
        "search": { "type": "boolean" },
        "graph": { "type": "boolean" },
        "backlinks": { "type": "boolean" },
        "rss": { "type": "boolean" },
        "shell": { "$ref": "#/$defs/shellOptions" },
        "navigation": { "$ref": "#/$defs/navigationOptions" },
        "modules": { "$ref": "#/$defs/moduleOptions" }
      }
    },
    "shellOptions": {
      "type": "object",
      "required": ["reader_mode", "default_palette", "left_rail", "right_rail"],
      "additionalProperties": false,
      "properties": {
        "reader_mode": { "type": "boolean" },
        "default_palette": { "type": "string", "enum": ["system", "light", "dark"] },
        "left_rail": { "type": "boolean" },
        "right_rail": { "type": "boolean" }
      }
    },
    "navigationOptions": {
      "type": "object",
      "required": ["explorer", "folder_click", "default_folder_state", "use_saved_state", "show_home", "show_recent", "show_folders", "show_tags", "show_graph"],
      "additionalProperties": false,
      "properties": {
        "explorer": { "type": "boolean" },
        "folder_click": { "type": "string", "enum": ["collapse", "link"] },
        "default_folder_state": { "type": "string", "enum": ["collapsed", "open"] },
        "use_saved_state": { "type": "boolean" },
        "show_home": { "type": "boolean" },
        "show_recent": { "type": "boolean" },
        "show_folders": { "type": "boolean" },
        "show_tags": { "type": "boolean" },
        "show_graph": { "type": "boolean" }
      }
    },
    "moduleOptions": {
      "type": "object",
      "required": ["toc", "graph", "backlinks", "outgoing_links"],
      "additionalProperties": false,
      "properties": {
        "toc": { "type": "boolean" },
        "graph": { "type": "boolean" },
        "backlinks": { "type": "boolean" },
        "outgoing_links": { "type": "boolean" }
      }
    },
    "renderContext": {
      "type": "object",
      "required": ["profile", "site_title", "language", "theme", "deploy_path"],
      "additionalProperties": false,
      "properties": {
        "profile": { "type": "string" },
        "site_title": { "type": "string" },
        "language": { "type": "string" },
        "theme": { "type": "string" },
        "base_url": { "type": ["string", "null"] },
        "deploy_path": { "type": "string" }
      }
    },
    "siteRoute": {
      "type": "object",
      "required": ["kind", "title", "slug", "url_path", "output_path"],
      "additionalProperties": false,
      "properties": {
        "kind": { "type": "string" },
        "source_path": { "type": ["string", "null"] },
        "title": { "type": "string" },
        "slug": { "type": "string" },
        "url_path": { "type": "string" },
        "output_path": { "type": "string" }
      }
    },
    "siteDiagnostic": {
      "type": "object",
      "required": ["level", "kind", "message"],
      "additionalProperties": false,
      "properties": {
        "level": { "type": "string" },
        "kind": { "type": "string" },
        "source_path": { "type": ["string", "null"] },
        "message": { "type": "string" }
      }
    },
    "noteIndexEntry": {
      "type": "object",
      "required": ["source_path", "title", "excerpt", "route", "document_path", "tags"],
      "additionalProperties": false,
      "properties": {
        "source_path": { "type": "string" },
        "title": { "type": "string" },
        "excerpt": { "type": "string" },
        "canonical_url": { "type": ["string", "null"] },
        "summary_image": { "type": ["string", "null"] },
        "route": { "$ref": "#/$defs/siteRoute" },
        "document_path": { "type": "string" },
        "tags": { "type": "array", "items": { "type": "string" } }
      }
    },
    "artifactPaths": {
      "type": "object",
      "required": ["route_manifest", "navigation_tree", "hover_previews", "recent_notes", "related_notes", "note_index", "invalidation", "schema", "typescript", "copied_assets"],
      "additionalProperties": false,
      "properties": {
        "route_manifest": { "type": "string" },
        "navigation_tree": { "type": "string" },
        "hover_previews": { "type": "string" },
        "recent_notes": { "type": "string" },
        "related_notes": { "type": "string" },
        "note_index": { "type": "string" },
        "search_index": { "type": ["string", "null"] },
        "graph": { "type": ["string", "null"] },
        "invalidation": { "type": "string" },
        "schema": { "type": "string" },
        "typescript": { "type": "string" },
        "copied_assets": { "type": "array", "items": { "type": "string" } }
      }
    }
  }
}"##
    .to_string()
}

fn frontend_bundle_typescript_definitions() -> &'static str {
    r"export interface FrontendBundleContractInfo {
  name: string;
  version: number;
}

export interface FrontendBundleProfile {
  name: string;
  title: string;
  deploy_path: string;
  base_url?: string | null;
  language: string;
  theme: string;
  search: boolean;
  graph: boolean;
  backlinks: boolean;
  rss: boolean;
  shell: FrontendBundleShell;
  navigation: FrontendBundleNavigation;
  modules: FrontendBundleModules;
}

export interface FrontendBundleShell {
  reader_mode: boolean;
  default_palette: 'system' | 'light' | 'dark';
  left_rail: boolean;
  right_rail: boolean;
}

export interface FrontendBundleNavigation {
  explorer: boolean;
  folder_click: 'collapse' | 'link';
  default_folder_state: 'collapsed' | 'open';
  use_saved_state: boolean;
  show_home: boolean;
  show_recent: boolean;
  show_folders: boolean;
  show_tags: boolean;
  show_graph: boolean;
}

export interface FrontendBundleModules {
  toc: boolean;
  graph: boolean;
  backlinks: boolean;
  outgoing_links: boolean;
}

export interface RenderContext {
  profile: string;
  site_title: string;
  language: string;
  theme: string;
  base_url?: string | null;
  deploy_path: string;
}

export interface SiteRoute {
  kind: string;
  source_path?: string | null;
  title: string;
  slug: string;
  url_path: string;
  output_path: string;
}

export interface SiteDiagnostic {
  level: string;
  kind: string;
  source_path?: string | null;
  message: string;
}

export interface HtmlRenderHeading {
  level: number;
  text: string;
  id: string;
}

export interface HtmlRenderDiagnostic {
  kind: string;
  message: string;
}

export interface RenderedEmbed {
  kind: string;
  source_path: string;
  target_path: string;
  url_path: string;
}

export interface FrontendBundleArtifactPaths {
  route_manifest: string;
  navigation_tree: string;
  hover_previews: string;
  recent_notes: string;
  related_notes: string;
  note_index: string;
  search_index?: string | null;
  graph?: string | null;
  invalidation: string;
  schema: string;
  typescript: string;
  copied_assets: string[];
}

export interface FrontendBundleNoteIndexEntry {
  source_path: string;
  title: string;
  excerpt: string;
  canonical_url?: string | null;
  summary_image?: string | null;
  route: SiteRoute;
  document_path: string;
  tags: string[];
}

export interface FrontendBundleNoteDocument {
  source_path: string;
  title: string;
  excerpt: string;
  description: string;
  canonical_url?: string | null;
  summary_image?: string | null;
  route: SiteRoute;
  body_html: string;
  headings: HtmlRenderHeading[];
  tags: string[];
  aliases: string[];
  outgoing_links: string[];
  backlinks: string[];
  breadcrumbs: string[];
  asset_paths: string[];
  embeds: RenderedEmbed[];
  diagnostics: HtmlRenderDiagnostic[];
  file_mtime: number;
}

export interface FrontendBundleContract {
  contract: FrontendBundleContractInfo;
  profile: FrontendBundleProfile;
  context: RenderContext;
  note_count: number;
  diagnostics: SiteDiagnostic[];
  routes: SiteRoute[];
  notes: FrontendBundleNoteIndexEntry[];
  artifacts: FrontendBundleArtifactPaths;
}

export interface FrontendBundleInvalidationReport {
  changed_files: string[];
  deleted_files: string[];
  changed_routes: string[];
  deleted_routes: string[];
  changed_assets: string[];
  deleted_assets: string[];
}
"
}

fn write_output_bytes_if_changed(path: &Path, contents: &[u8]) -> Result<bool, AppError> {
    if fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    fs::write(path, contents).map_err(AppError::operation)?;
    Ok(true)
}

fn remove_stale_output_files(
    output_dir: &Path,
    expected_files: &BTreeSet<String>,
) -> Result<Vec<String>, AppError> {
    if !output_dir.exists() {
        return Ok(Vec::new());
    }
    let mut deleted = Vec::new();
    remove_stale_output_files_recursive(output_dir, output_dir, expected_files, &mut deleted)?;
    deleted.sort();
    Ok(deleted)
}

fn remove_stale_output_files_recursive(
    output_dir: &Path,
    directory: &Path,
    expected_files: &BTreeSet<String>,
    deleted: &mut Vec<String>,
) -> Result<bool, AppError> {
    let mut has_entries = false;
    for entry in fs::read_dir(directory).map_err(AppError::operation)? {
        let entry = entry.map_err(AppError::operation)?;
        let path = entry.path();
        if entry.file_type().map_err(AppError::operation)?.is_dir() {
            let child_has_entries =
                remove_stale_output_files_recursive(output_dir, &path, expected_files, deleted)?;
            if child_has_entries {
                has_entries = true;
            } else {
                fs::remove_dir(&path).map_err(AppError::operation)?;
            }
            continue;
        }

        let relative = display_path(path.strip_prefix(output_dir).map_err(AppError::operation)?);
        if expected_files.contains(&relative) {
            has_entries = true;
            continue;
        }
        fs::remove_file(&path).map_err(AppError::operation)?;
        deleted.push(relative);
    }
    Ok(has_entries)
}

fn resolve_site_theme(paths: &VaultPaths, theme: &str) -> Result<ResolvedSiteTheme, AppError> {
    let trimmed = theme.trim();
    if trimmed.is_empty() || trimmed == "default" {
        return Ok(ResolvedSiteTheme::default());
    }

    let theme_dir = resolve_site_theme_dir(paths, trimmed)?;
    let relative_theme_dir = theme_dir
        .strip_prefix(paths.vault_root())
        .map_err(AppError::operation)?;
    let relative_theme_dir = PathBuf::from(display_path(relative_theme_dir));
    let css_assets = if theme_dir.join("theme.css").is_file() {
        vec![relative_theme_dir.join("theme.css")]
    } else {
        Vec::new()
    };
    let js_assets = if theme_dir.join("theme.js").is_file() {
        vec![relative_theme_dir.join("theme.js")]
    } else {
        Vec::new()
    };

    Ok(ResolvedSiteTheme {
        css_assets,
        js_assets,
        head: read_optional_theme_partial(&theme_dir.join("head.html"))?,
        header: read_optional_theme_partial(&theme_dir.join("header.html"))?,
        nav: read_optional_theme_partial(&theme_dir.join("nav.html"))?,
        toolbar: read_optional_theme_partial(&theme_dir.join("toolbar.html"))?,
        left_rail: read_optional_theme_partial(&theme_dir.join("left_rail.html"))?,
        right_rail: read_optional_theme_partial(&theme_dir.join("right_rail.html"))?,
        footer: read_optional_theme_partial(&theme_dir.join("footer.html"))?,
        note_before: read_optional_theme_partial(&theme_dir.join("note_before.html"))?,
        note_after: read_optional_theme_partial(&theme_dir.join("note_after.html"))?,
    })
}

fn resolve_site_theme_dir(paths: &VaultPaths, theme: &str) -> Result<PathBuf, AppError> {
    let candidates = if theme.contains('/') || theme.contains('\\') || theme.starts_with('.') {
        vec![paths.vault_root().join(theme)]
    } else {
        vec![
            paths.vault_root().join(".vulcan/site/themes").join(theme),
            paths.vault_root().join(theme),
        ]
    };
    let Some(theme_dir) = candidates.into_iter().find(|candidate| candidate.exists()) else {
        return Err(AppError::operation(format!(
            "site theme `{theme}` was not found in `.vulcan/site/themes/{theme}` or `{theme}`"
        )));
    };
    if !theme_dir.is_dir() {
        return Err(AppError::operation(format!(
            "site theme `{theme}` must resolve to a directory, got `{}`",
            theme_dir.display()
        )));
    }
    Ok(theme_dir)
}

fn read_optional_theme_partial(path: &Path) -> Result<Option<String>, AppError> {
    if !path.is_file() {
        return Ok(None);
    }
    fs::read_to_string(path)
        .map(Some)
        .map_err(AppError::operation)
}

fn normalize_site_deploy_path(value: Option<&str>) -> Result<String, AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(String::new());
    };
    let segments = value
        .replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(|segment| {
            if segment == ".." {
                Err(AppError::operation(format!(
                    "site deploy_path cannot contain parent traversal segments: `{value}`"
                )))
            } else {
                Ok(encode_url_segment(segment))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if segments.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("/{}", segments.join("/")))
    }
}

fn site_root_href(deploy_path: &str) -> String {
    if deploy_path.is_empty() {
        "/".to_string()
    } else {
        format!("{deploy_path}/")
    }
}

fn prefixed_site_path(deploy_path: &str, path: &str) -> String {
    let path = if path.is_empty() {
        "/".to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    if deploy_path.is_empty() {
        path
    } else if path == "/" {
        site_root_href(deploy_path)
    } else {
        format!("{deploy_path}{path}")
    }
}

fn normalize_relative_path(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref()
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(PathBuf::from(part)),
            _ => None,
        })
        .fold(PathBuf::new(), |mut acc, component| {
            acc.push(component);
            acc
        })
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn display_path(path: &Path) -> String {
    normalize_path(&path.to_string_lossy())
}

fn trim_markdown_extension(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

fn slugify_path(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(slugify_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn slugify_segment(segment: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for character in segment.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            previous_dash = false;
        } else if !previous_dash {
            slug.push('-');
            previous_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "note".to_string()
    } else {
        slug
    }
}

fn normalize_tag(tag: &str) -> String {
    tag.trim().trim_start_matches('#').to_ascii_lowercase()
}

fn excerpt_from_markdown(source: &str) -> String {
    let mut text = String::new();
    let mut in_frontmatter = false;
    for line in source.lines() {
        if line.trim() == "---" {
            in_frontmatter = !in_frontmatter;
            continue;
        }
        if in_frontmatter || line.trim().is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(line.trim());
        if text.len() >= 220 {
            break;
        }
    }
    let simplified = text
        .replace("[[", "")
        .replace("]]", "")
        .replace(['#', '`', '*', '!', '[', ']', '(', ')'], "");
    simplified.chars().take(220).collect::<String>()
}

fn search_text_from_markdown(source: &str) -> String {
    // This keeps the search corpus close to the original note wording while staying much cheaper
    // than reparsing emitted HTML during asset generation.
    let mut text = String::new();
    let mut in_frontmatter = false;
    let mut frontmatter_boundary_count = 0usize;
    let mut in_fenced_block = false;
    let mut in_inline_code = false;
    let mut previous_was_space = true;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed == "---" && frontmatter_boundary_count < 2 && text.is_empty() && !in_fenced_block
        {
            in_frontmatter = !in_frontmatter;
            frontmatter_boundary_count += 1;
            continue;
        }
        if in_frontmatter {
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fenced_block = !in_fenced_block;
            continue;
        }
        if in_fenced_block {
            continue;
        }
        for ch in line.chars() {
            if ch == '`' {
                in_inline_code = !in_inline_code;
                if !previous_was_space {
                    text.push(' ');
                    previous_was_space = true;
                }
                continue;
            }
            if !in_inline_code
                && matches!(
                    ch,
                    '[' | ']' | '(' | ')' | '<' | '>' | '#' | '*' | '_' | '!' | '|' | ':' | '-'
                )
            {
                if !previous_was_space {
                    text.push(' ');
                    previous_was_space = true;
                }
                continue;
            }
            if ch == '&' {
                if !previous_was_space {
                    text.push(' ');
                    previous_was_space = true;
                }
                continue;
            }
            if ch.is_whitespace() {
                if !previous_was_space {
                    text.push(' ');
                    previous_was_space = true;
                }
                continue;
            }
            text.push(ch);
            previous_was_space = false;
        }
        if !previous_was_space {
            text.push(' ');
            previous_was_space = true;
        }
    }
    text.trim()
        .replace(" .", ".")
        .replace(" ,", ",")
        .replace(" !", "!")
        .replace(" ?", "?")
}

fn for_each_search_token(text: &str, mut visit: impl FnMut(&str)) {
    let mut current = String::new();
    for ch in text.chars() {
        match ch {
            _ if ch.is_alphanumeric() => current.extend(ch.to_lowercase()),
            _ if !current.is_empty() => {
                visit(&current);
                current.clear();
            }
            _ => {}
        }
    }
    if !current.is_empty() {
        visit(&current);
    }
}

fn accumulate_search_terms(
    frequencies: &mut BTreeMap<String, u16>,
    text: &str,
    weight: u16,
) -> usize {
    if weight == 0 {
        return 0;
    }
    let mut count = 0usize;
    for_each_search_token(text, |token| {
        count += 1;
        let entry = frequencies.entry(token.to_string()).or_insert(0);
        *entry = entry.saturating_add(weight);
    });
    count
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_xml(text: &str) -> String {
    escape_html(text).replace('\'', "&apos;")
}

fn encode_url_path(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(encode_url_segment(&part.to_string_lossy())),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn encode_url_segment(segment: &str) -> String {
    let mut encoded = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to string cannot fail");
        }
    }
    encoded
}

fn site_link_policy_level(policy: SiteLinkPolicyConfig) -> &'static str {
    match policy {
        SiteLinkPolicyConfig::Error => "error",
        SiteLinkPolicyConfig::Warn
        | SiteLinkPolicyConfig::DropLink
        | SiteLinkPolicyConfig::RenderPlainText => "warn",
    }
}

#[cfg(test)]
mod tests;
