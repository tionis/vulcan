#![allow(
    clippy::format_collect,
    clippy::struct_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

use crate::export::{prepare_export_data, ExportLinkRecord, ExportedNoteDocument};
use crate::AppError;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use vulcan_core::config::{
    load_vault_config, ContentTransformRuleConfig, SiteAssetPolicyConfig,
    SiteAssetPolicyModeConfig, SiteDataviewJsPolicyConfig, SiteLinkPolicyConfig, SiteProfileConfig,
};
use vulcan_core::graph::resolve_note_reference;
use vulcan_core::html::{
    render_vault_html, HtmlDataviewJsPolicy, HtmlLinkTargets, HtmlRenderDiagnostic,
    HtmlRenderHeading, HtmlRenderOptions,
};
use vulcan_core::properties::NoteRecord;
use vulcan_core::query::{execute_query_report, QueryAst, QueryReport};
use vulcan_core::search::export_static_search_index;
use vulcan_core::{export_graph, VaultPaths};

const DEFAULT_PAGE_TITLE_TEMPLATE: &str = "{page} | {site}";

const DEFAULT_THEME_CSS: &str = concat!(
    ":root {\n",
    "  color-scheme: light dark;\n",
    "  --bg: #f5f1e8;\n",
    "  --bg-strong: #ece3d1;\n",
    "  --panel: rgba(255, 252, 247, 0.9);\n",
    "  --text: #1f1a14;\n",
    "  --muted: #625448;\n",
    "  --accent: #0d6b57;\n",
    "  --accent-strong: #094c3d;\n",
    "  --border: rgba(31, 26, 20, 0.14);\n",
    "  --shadow: 0 20px 40px rgba(31, 26, 20, 0.08);\n",
    "  --code-bg: rgba(31, 26, 20, 0.06);\n",
    "  --link: #0f5e9c;\n",
    "}\n",
    "@media (prefers-color-scheme: dark) {\n",
    "  :root {\n",
    "    --bg: #161515;\n",
    "    --bg-strong: #201e1c;\n",
    "    --panel: rgba(30, 28, 26, 0.92);\n",
    "    --text: #f0eadf;\n",
    "    --muted: #bcaf9c;\n",
    "    --accent: #6dc7a7;\n",
    "    --accent-strong: #91e0c4;\n",
    "    --border: rgba(240, 234, 223, 0.14);\n",
    "    --shadow: 0 20px 40px rgba(0, 0, 0, 0.35);\n",
    "    --code-bg: rgba(240, 234, 223, 0.08);\n",
    "    --link: #8ec7ff;\n",
    "  }\n",
    "}\n",
    "html[data-theme='light'] { color-scheme: light; }\n",
    "html[data-theme='dark'] { color-scheme: dark; }\n",
    "* { box-sizing: border-box; }\n",
    "body { margin: 0; font-family: Georgia, 'Iowan Old Style', 'Palatino Linotype', serif; background: radial-gradient(circle at top, var(--bg-strong), var(--bg)); color: var(--text); }\n",
    "a { color: var(--link); }\n",
    "img, video { max-width: 100%; height: auto; }\n",
    "code, pre { font-family: 'IBM Plex Mono', 'SFMono-Regular', monospace; background: var(--code-bg); }\n",
    "pre { padding: 1rem; border-radius: 1rem; overflow: auto; }\n",
    ".site-shell { max-width: 1180px; margin: 0 auto; padding: 2rem 1.25rem 4rem; }\n",
    ".site-header { display: flex; gap: 1rem; justify-content: space-between; align-items: center; margin-bottom: 2rem; }\n",
    ".site-brand h1 { margin: 0; font-size: clamp(2rem, 4vw, 3rem); }\n",
    ".site-brand { display: flex; gap: 0.85rem; align-items: center; }\n",
    ".site-brand p { margin: 0.4rem 0 0; color: var(--muted); }\n",
    ".site-brand-mark { width: 3rem; height: 3rem; border-radius: 0.9rem; object-fit: cover; border: 1px solid var(--border); box-shadow: var(--shadow); background: rgba(255,255,255,0.35); }\n",
    ".site-toolbar { display: flex; gap: 0.75rem; align-items: center; flex-wrap: wrap; }\n",
    ".site-toolbar a, .site-toolbar button { border: 1px solid var(--border); background: var(--panel); color: var(--text); border-radius: 999px; padding: 0.55rem 0.9rem; text-decoration: none; cursor: pointer; box-shadow: var(--shadow); }\n",
    ".site-layout { display: grid; grid-template-columns: minmax(0, 1fr) 280px; gap: 1.5rem; }\n",
    ".site-main, .site-sidebar > section, .site-listing, .site-search-card, .site-graph-card { background: var(--panel); border: 1px solid var(--border); border-radius: 1.5rem; box-shadow: var(--shadow); }\n",
    ".site-main { padding: 1.5rem; }\n",
    ".site-sidebar { display: grid; gap: 1rem; align-content: start; }\n",
    ".site-sidebar > section { padding: 1rem 1.1rem; }\n",
    ".site-meta, .site-breadcrumbs, .site-footer, .site-listing p, .site-empty { color: var(--muted); }\n",
    ".site-breadcrumbs { display: flex; gap: 0.5rem; flex-wrap: wrap; font-size: 0.95rem; margin-bottom: 1rem; }\n",
    ".site-listing, .site-search-card, .site-graph-card { padding: 1.25rem; }\n",
    ".site-listing ul, .site-sidebar ul, .site-search-results { padding-left: 1.2rem; margin: 0.75rem 0 0; }\n",
    ".site-card-grid { display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); }\n",
    ".site-card { border: 1px solid var(--border); border-radius: 1.2rem; padding: 1rem; background: rgba(255,255,255,0.22); }\n",
    ".site-footer { margin-top: 2rem; font-size: 0.95rem; }\n",
    ".site-search-input { width: 100%; padding: 0.8rem 1rem; border-radius: 999px; border: 1px solid var(--border); background: rgba(255,255,255,0.55); color: var(--text); }\n",
    ".site-pill-list { display: flex; gap: 0.5rem; flex-wrap: wrap; list-style: none; padding: 0; margin: 0.9rem 0 0; }\n",
    ".site-pill-list a { display: inline-flex; border: 1px solid var(--border); padding: 0.35rem 0.75rem; border-radius: 999px; text-decoration: none; }\n",
    ".site-inline-nav { display: flex; gap: 0.8rem; justify-content: space-between; margin-top: 2rem; flex-wrap: wrap; }\n",
    ".site-callout { border-left: 4px solid var(--accent); padding-left: 1rem; color: var(--muted); }\n",
    ".site-diagnostics { margin-top: 1rem; border-radius: 1rem; border: 1px solid rgba(178, 69, 54, 0.35); background: rgba(178, 69, 54, 0.08); padding: 1rem; }\n",
    ".site-live-overlay { position: fixed; right: 1rem; bottom: 1rem; z-index: 9999; background: rgba(20, 20, 20, 0.92); color: #fff; padding: 0.9rem 1rem; border-radius: 1rem; max-width: 24rem; box-shadow: 0 18px 36px rgba(0,0,0,0.35); }\n",
    "@media (max-width: 900px) { .site-layout { grid-template-columns: 1fr; } .site-header { flex-direction: column; align-items: start; } }\n"
);

const DEFAULT_THEME_JS: &str = concat!(
    "(() => {\n",
    "  const root = document.documentElement;\n",
    "  const storedTheme = localStorage.getItem('vulcan-site-theme');\n",
    "  if (storedTheme) root.dataset.theme = storedTheme;\n",
    "  document.addEventListener('click', event => {\n",
    "    const button = event.target.closest('[data-theme-toggle]');\n",
    "    if (!button) return;\n",
    "    const next = root.dataset.theme === 'dark' ? 'light' : 'dark';\n",
    "    root.dataset.theme = next;\n",
    "    localStorage.setItem('vulcan-site-theme', next);\n",
    "  });\n",
    "  const searchInput = document.querySelector('[data-site-search-input]');\n",
    "  const searchResults = document.querySelector('[data-site-search-results]');\n",
    "  const searchAsset = document.body.dataset.searchAsset;\n",
    "  if (searchInput && searchResults && searchAsset) {\n",
    "    let entries = [];\n",
    "    fetch(searchAsset).then(response => response.ok ? response.json() : []).then(payload => { entries = payload.entries || []; });\n",
    "    const render = () => {\n",
    "      const query = searchInput.value.trim().toLowerCase();\n",
    "      const hits = query ? entries.filter(entry => (entry.title + ' ' + entry.content + ' ' + entry.tags.join(' ')).toLowerCase().includes(query)).slice(0, 20) : [];\n",
    "      searchResults.innerHTML = hits.map(hit => `<li><a href=\"${hit.url}\">${hit.title}</a><div>${hit.excerpt}</div></li>`).join('') || (query ? '<li>No matches</li>' : '');\n",
    "    };\n",
    "    searchInput.addEventListener('input', render);\n",
    "    document.addEventListener('keydown', event => {\n",
    "      if (event.key === '/' && !event.metaKey && !event.ctrlKey && !event.altKey) {\n",
    "        if (document.activeElement && /input|textarea/i.test(document.activeElement.tagName)) return;\n",
    "        event.preventDefault();\n",
    "        searchInput.focus();\n",
    "      }\n",
    "    });\n",
    "  }\n",
    "  let liveVersion = null;\n",
    "  const liveUrl = '/__vulcan_site/live-reload.json';\n",
    "  const overlayId = 'vulcan-site-live-overlay';\n",
    "  const ensureOverlay = message => {\n",
    "    let overlay = document.getElementById(overlayId);\n",
    "    if (!overlay) {\n",
    "      overlay = document.createElement('div');\n",
    "      overlay.id = overlayId;\n",
    "      overlay.className = 'site-live-overlay';\n",
    "      document.body.appendChild(overlay);\n",
    "    }\n",
    "    overlay.textContent = message;\n",
    "  };\n",
    "  const pollLiveReload = () => {\n",
    "    fetch(liveUrl, { cache: 'no-store' }).then(response => response.ok ? response.json() : null).then(payload => {\n",
    "      if (!payload) return;\n",
    "      if (liveVersion === null) { liveVersion = payload.version; }\n",
    "      else if (payload.version !== liveVersion) { window.location.reload(); }\n",
    "      if (payload.last_error) ensureOverlay(payload.last_error);\n",
    "    }).catch(() => {});\n",
    "  };\n",
    "  window.setInterval(pollLiveReload, 1200);\n",
    "})();\n"
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderContext {
    pub profile: String,
    pub site_title: String,
    pub language: String,
    pub theme: String,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SiteRoute {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub title: String,
    pub slug: String,
    pub url_path: String,
    pub output_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderedEmbed {
    pub kind: String,
    pub source_path: String,
    pub target_path: String,
    pub url_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RenderedNote {
    pub source_path: String,
    pub title: String,
    pub excerpt: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_image: Option<String>,
    pub route: SiteRoute,
    pub html: String,
    pub headings: Vec<HtmlRenderHeading>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub outgoing_links: Vec<String>,
    pub backlinks: Vec<String>,
    pub breadcrumbs: Vec<String>,
    pub asset_paths: Vec<String>,
    pub embeds: Vec<RenderedEmbed>,
    pub diagnostics: Vec<HtmlRenderDiagnostic>,
    pub file_mtime: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SiteDiagnostic {
    pub level: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SiteProfileListEntry {
    pub name: String,
    pub title: String,
    pub output_dir: String,
    pub note_count: usize,
    pub search: bool,
    pub graph: bool,
    pub backlinks: bool,
    pub rss: bool,
    pub theme: String,
    pub implicit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteBuildRequest {
    pub profile: Option<String>,
    pub output_dir: Option<PathBuf>,
    pub clean: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SiteBuildReport {
    pub profile: String,
    pub output_dir: String,
    pub dry_run: bool,
    pub clean: bool,
    pub note_count: usize,
    pub page_count: usize,
    pub asset_count: usize,
    pub search_enabled: bool,
    pub graph_enabled: bool,
    pub rss_enabled: bool,
    pub diagnostics: Vec<SiteDiagnostic>,
    pub routes: Vec<SiteRoute>,
    pub rendered_notes: Vec<RenderedNote>,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SiteDoctorReport {
    pub profile: String,
    pub note_count: usize,
    pub diagnostics: Vec<SiteDiagnostic>,
    pub routes: Vec<SiteRoute>,
}

#[derive(Debug, Clone)]
struct ResolvedSiteProfile {
    name: String,
    title: String,
    page_title_template: String,
    output_dir: PathBuf,
    base_url: Option<String>,
    home: Option<String>,
    language: String,
    theme: String,
    search: bool,
    graph: bool,
    backlinks: bool,
    rss: bool,
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
    content_transform_rules: Option<Vec<ContentTransformRuleConfig>>,
    implicit: bool,
}

#[derive(Debug, Clone)]
struct SitePlan {
    profile: ResolvedSiteProfile,
    notes: Vec<ExportedNoteDocument>,
    links: Vec<ExportLinkRecord>,
    routes: Vec<SiteRoute>,
    diagnostics: Vec<SiteDiagnostic>,
}

#[derive(Debug, Clone, Serialize)]
struct SearchIndexEntry {
    title: String,
    url: String,
    excerpt: String,
    content: String,
    tags: Vec<String>,
}

pub fn build_site_profiles_report(
    paths: &VaultPaths,
) -> Result<Vec<SiteProfileListEntry>, AppError> {
    let profile_names = available_site_profile_names(paths);
    profile_names
        .into_iter()
        .map(|name| {
            let profile = resolve_site_profile(paths, Some(name.as_str()), None)?;
            let note_count = select_site_notes(paths, &profile)?.len();
            Ok(SiteProfileListEntry {
                name: profile.name.clone(),
                title: profile.title.clone(),
                output_dir: display_path(&profile.output_dir),
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
    let plan = plan_site(
        paths,
        request.profile.as_deref(),
        request.output_dir.as_deref(),
    )?;
    if request.clean && !request.dry_run && plan.profile.output_dir.exists() {
        fs::remove_dir_all(&plan.profile.output_dir).map_err(AppError::operation)?;
    }
    let output_dir = plan.profile.output_dir.clone();
    let mut rendered_notes = render_site_notes(paths, &plan)?;
    rendered_notes.sort_by(|left, right| left.route.url_path.cmp(&right.route.url_path));

    let routes_by_path = rendered_notes
        .iter()
        .map(|note| (note.source_path.clone(), note.route.clone()))
        .collect::<HashMap<_, _>>();
    let route_urls = rendered_notes
        .iter()
        .map(|note| (note.source_path.clone(), note.route.url_path.clone()))
        .collect::<HashMap<_, _>>();
    let mut files = BTreeSet::<String>::new();
    let mut asset_count = 0_usize;

    if !request.dry_run {
        fs::create_dir_all(&output_dir).map_err(AppError::operation)?;
    }

    let context = RenderContext {
        profile: plan.profile.name.clone(),
        site_title: plan.profile.title.clone(),
        language: plan.profile.language.clone(),
        theme: plan.profile.theme.clone(),
        base_url: plan.profile.base_url.clone(),
    };

    let tag_index = build_tag_index(&rendered_notes);
    let folder_index = build_folder_index(&rendered_notes);
    let home_note = resolve_home_note(&plan.profile, &rendered_notes);

    for (index, note) in rendered_notes.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .and_then(|value| rendered_notes.get(value));
        let next = rendered_notes.get(index + 1);
        let html = render_note_document(
            &context,
            note,
            previous,
            next,
            &plan.profile,
            &route_urls,
            &tag_index,
            &folder_index,
            home_note,
        );
        if !request.dry_run {
            let path = output_dir.join(&note.route.output_path);
            write_output_file(&path, &html)?;
        }
        files.insert(note.route.output_path.clone());
    }

    let asset_links = collect_asset_links(&plan.links);
    for (source_path, href) in &asset_links {
        let destination = asset_output_path(&output_dir, source_path);
        if !request.dry_run {
            copy_asset(paths, source_path, &destination)?;
        }
        files.insert(display_path(
            destination
                .strip_prefix(&output_dir)
                .unwrap_or(&destination),
        ));
        let _ = href;
        asset_count += 1;
    }

    let summary_image_assets = rendered_notes
        .iter()
        .filter_map(|note| note.summary_image.as_deref())
        .filter_map(summary_image_source_path)
        .collect::<BTreeSet<_>>();
    for summary_image in &summary_image_assets {
        let destination = output_dir
            .join("assets")
            .join(normalize_relative_path(summary_image));
        if !request.dry_run {
            copy_file_from_vault(paths, summary_image, &destination)?;
        }
        files.insert(display_path(
            destination
                .strip_prefix(&output_dir)
                .unwrap_or(&destination),
        ));
        asset_count += 1;
    }

    for extra_asset in plan
        .profile
        .extra_css
        .iter()
        .chain(plan.profile.extra_js.iter())
        .chain(plan.profile.favicon.iter())
        .chain(plan.profile.logo.iter())
    {
        let relative = normalize_relative_path(extra_asset);
        let destination = output_dir.join("assets").join(&relative);
        if !request.dry_run {
            copy_file_from_vault(paths, extra_asset, &destination)?;
        }
        files.insert(display_path(
            destination
                .strip_prefix(&output_dir)
                .unwrap_or(&destination),
        ));
        asset_count += 1;
    }

    for extra_pattern in &plan.profile.asset_policy.include_folders {
        for asset in collect_extra_assets(paths, extra_pattern)? {
            let destination = output_dir
                .join("assets")
                .join(normalize_relative_path(&asset));
            if !request.dry_run {
                copy_file_from_vault(paths, &asset, &destination)?;
            }
            files.insert(display_path(
                destination
                    .strip_prefix(&output_dir)
                    .unwrap_or(&destination),
            ));
            asset_count += 1;
        }
    }

    let css_path = output_dir.join("assets/vulcan-site.css");
    let js_path = output_dir.join("assets/vulcan-site.js");
    if !request.dry_run {
        write_output_file(&css_path, DEFAULT_THEME_CSS)?;
        write_output_file(&js_path, DEFAULT_THEME_JS)?;
    }
    files.insert("assets/vulcan-site.css".to_string());
    files.insert("assets/vulcan-site.js".to_string());

    let manifest = serde_json::to_string_pretty(&plan.routes).map_err(AppError::operation)?;
    let manifest_path = output_dir.join("assets/route-manifest.json");
    if !request.dry_run {
        write_output_file(&manifest_path, &manifest)?;
    }
    files.insert("assets/route-manifest.json".to_string());

    let hover_manifest = build_hover_manifest(&rendered_notes);
    let hover_manifest_json =
        serde_json::to_string_pretty(&hover_manifest).map_err(AppError::operation)?;
    let hover_path = output_dir.join("assets/hover-previews.json");
    if !request.dry_run {
        write_output_file(&hover_path, &hover_manifest_json)?;
    }
    files.insert("assets/hover-previews.json".to_string());

    if plan.profile.search {
        let search_index = build_search_index(paths, &plan.notes, &routes_by_path)?;
        let search_json =
            serde_json::to_string_pretty(&search_index).map_err(AppError::operation)?;
        let search_path = output_dir.join("assets/search-index.json");
        if !request.dry_run {
            write_output_file(&search_path, &search_json)?;
            write_output_file(
                &output_dir.join("search/index.html"),
                &render_generic_page(
                    &context,
                    "Search",
                    "Find published notes with keyboard-first search.",
                    concat!(
                        "<section class=\"site-search-card\"><input class=\"site-search-input\" data-site-search-input placeholder=\"Type to search…\" />",
                        "<ol class=\"site-search-results\" data-site-search-results></ol></section>"
                    ),
                    &plan.profile,
                    true,
                    "/search/",
                ),
            )?;
        }
        files.insert("assets/search-index.json".to_string());
        files.insert("search/index.html".to_string());
    }

    if plan.profile.graph {
        let graph_json = build_graph_asset(paths, &rendered_notes)?;
        let graph_path = output_dir.join("assets/graph.json");
        if !request.dry_run {
            write_output_file(&graph_path, &graph_json)?;
            let graph_card = format!(
                concat!(
                    "<section class=\"site-graph-card\"><h2>Published graph</h2>",
                    "<p>This build includes a filtered graph asset reused by later WebUI/wiki work.</p>",
                    "<pre><code>{}</code></pre></section>"
                ),
                escape_html(&graph_json)
            );
            write_output_file(
                &output_dir.join("graph/index.html"),
                &render_generic_page(
                    &context,
                    "Graph",
                    "Graph export",
                    &graph_card,
                    &plan.profile,
                    false,
                    "/graph/",
                ),
            )?;
        }
        files.insert("assets/graph.json".to_string());
        files.insert("graph/index.html".to_string());
    }

    if plan.profile.rss && plan.profile.base_url.is_some() {
        let rss = build_rss_document(&context, &rendered_notes);
        if !request.dry_run {
            write_output_file(&output_dir.join("rss.xml"), &rss)?;
        }
        files.insert("rss.xml".to_string());
    }

    let folder_pages = render_folder_pages(&context, &folder_index, &plan.profile);
    for (relative_path, body) in folder_pages {
        if !request.dry_run {
            write_output_file(&output_dir.join(&relative_path), &body)?;
        }
        files.insert(relative_path);
    }

    let tag_pages = render_tag_pages(&context, &tag_index, &plan.profile);
    for (relative_path, body) in tag_pages {
        if !request.dry_run {
            write_output_file(&output_dir.join(&relative_path), &body)?;
        }
        files.insert(relative_path);
    }

    let recent_html = render_recent_page(&context, &rendered_notes, &plan.profile);
    if !request.dry_run {
        write_output_file(&output_dir.join("recent/index.html"), &recent_html)?;
    }
    files.insert("recent/index.html".to_string());

    if let Some(home) = home_note.as_ref() {
        let home_html = render_home_page(
            &context,
            home,
            &plan.profile,
            &route_urls,
            &tag_index,
            &folder_index,
        );
        if !request.dry_run {
            write_output_file(&output_dir.join("index.html"), &home_html)?;
        }
    } else {
        let body = render_listing_cards(&rendered_notes);
        if !request.dry_run {
            write_output_file(
                &output_dir.join("index.html"),
                &render_generic_page(
                    &context,
                    &plan.profile.title,
                    "Published notes",
                    &body,
                    &plan.profile,
                    false,
                    "/",
                ),
            )?;
        }
    }
    files.insert("index.html".to_string());

    if let Some(base_url) = context.base_url.as_deref() {
        let sitemap = build_sitemap(base_url, &files, &rendered_notes);
        if !request.dry_run {
            write_output_file(&output_dir.join("sitemap.xml"), &sitemap)?;
        }
        files.insert("sitemap.xml".to_string());
    }

    let file_list = files.into_iter().collect::<Vec<_>>();
    Ok(SiteBuildReport {
        profile: plan.profile.name,
        output_dir: display_path(&output_dir),
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
        home: raw.home.clone(),
        language: raw.language.clone().unwrap_or_else(|| "en".to_string()),
        theme: raw.theme.clone().unwrap_or_else(|| "default".to_string()),
        search: raw.search.unwrap_or(true),
        graph: raw.graph.unwrap_or(true),
        backlinks: raw.backlinks.unwrap_or(true),
        rss: raw.rss.unwrap_or(false),
        favicon: raw.favicon.clone(),
        logo: raw.logo.clone(),
        extra_css: raw.extra_css.clone(),
        extra_js: raw.extra_js.clone(),
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
    let notes = select_site_notes(paths, &profile)?;
    let query = build_profile_query_ast(&profile)?;
    let report = QueryReport { query, notes };
    let prepared = prepare_export_data(
        paths,
        &report,
        None,
        profile.content_transform_rules.as_deref(),
    )
    .map_err(AppError::operation)?;
    let routes = plan_note_routes(&prepared.notes, &profile.name);
    let diagnostics =
        collect_site_diagnostics(paths, &profile, &prepared.notes, &prepared.links, &routes);
    Ok(SitePlan {
        profile,
        notes: prepared.notes,
        links: prepared.links,
        routes,
        diagnostics,
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
) -> Result<Vec<NoteRecord>, AppError> {
    let all_report = execute_query_report(
        paths,
        QueryAst::from_dsl("from notes").map_err(AppError::operation)?,
    )
    .map_err(AppError::operation)?;
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
    Ok(notes)
}

fn plan_note_routes(notes: &[ExportedNoteDocument], profile_name: &str) -> Vec<SiteRoute> {
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
                "/notes/".to_string()
            } else {
                format!("/notes/{}/", route_segments.join("/"))
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
    notes: &[ExportedNoteDocument],
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

fn render_site_notes(paths: &VaultPaths, plan: &SitePlan) -> Result<Vec<RenderedNote>, AppError> {
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
    let asset_hrefs = collect_asset_links(&plan.links);
    let note_hrefs = route_map
        .iter()
        .map(|(path, route)| (path.clone(), route.url_path.clone()))
        .collect::<HashMap<_, _>>();
    let tag_hrefs = build_tag_href_map(&plan.notes);
    let links_by_source = links_by_source(&plan.links);
    let backlinks = backlinks_by_target(&plan.links, &published_paths);
    let link_targets = HtmlLinkTargets {
        note_hrefs,
        asset_hrefs,
        tag_hrefs,
    };

    plan.notes
        .iter()
        .filter_map(|note| {
            route_map
                .get(&note.note.document_path)
                .map(|route| (note, route))
        })
        .map(|(note, route)| {
            let source_links = links_by_source
                .get(&note.note.document_path)
                .cloned()
                .unwrap_or_default();
            let adjusted = apply_link_policy_to_source(
                &note.content,
                &source_links,
                &published_paths,
                plan.profile.link_policy,
            );
            let rendered = render_vault_html(
                paths,
                &adjusted,
                &HtmlRenderOptions {
                    source_path: Some(&note.note.document_path),
                    full_document: true,
                    link_targets: Some(&link_targets),
                    dataview_js_policy: match plan.profile.dataview_js {
                        SiteDataviewJsPolicyConfig::Off => HtmlDataviewJsPolicy::Off,
                        SiteDataviewJsPolicyConfig::Static => HtmlDataviewJsPolicy::Static,
                    },
                    max_embed_depth: 4,
                },
            );
            let diagnostics = rendered.diagnostics.clone();
            let title = note_title(&note.note, &plan.profile.name, rendered.title.as_deref());
            let excerpt = excerpt_from_markdown(&adjusted);
            let outgoing_links = source_links
                .iter()
                .filter_map(|link| link.resolved_target_path.clone())
                .filter(|path| published_paths.contains(path))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let asset_paths = source_links
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
                .collect::<Vec<_>>();
            let embeds = source_links
                .iter()
                .filter_map(|link| {
                    if !link.link_kind.eq_ignore_ascii_case("embed") {
                        return None;
                    }
                    let target_path = link.resolved_target_path.clone()?;
                    let url_path = if link
                        .resolved_target_extension
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case("md"))
                    {
                        route_map.get(&target_path).map_or_else(
                            || {
                                collect_asset_links(&plan.links)
                                    .get(&target_path)
                                    .cloned()
                                    .unwrap_or_default()
                            },
                            |route| route.url_path.clone(),
                        )
                    } else {
                        collect_asset_links(&plan.links)
                            .get(&target_path)
                            .cloned()
                            .unwrap_or_default()
                    };
                    Some(RenderedEmbed {
                        kind: link.link_kind.clone(),
                        source_path: note.note.document_path.clone(),
                        target_path,
                        url_path,
                    })
                })
                .collect::<Vec<_>>();
            let description = frontmatter_override(&note.note, &plan.profile.name, "description")
                .unwrap_or_else(|| excerpt.clone());
            let canonical_url =
                frontmatter_override(&note.note, &plan.profile.name, "canonical_url");
            let summary_image =
                frontmatter_override(&note.note, &plan.profile.name, "summary_image");
            Ok(RenderedNote {
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
                outgoing_links,
                backlinks: backlinks
                    .get(&note.note.document_path)
                    .cloned()
                    .unwrap_or_default(),
                breadcrumbs: breadcrumbs_for_path(&note.note.document_path),
                asset_paths,
                embeds,
                diagnostics,
                file_mtime: note.note.file_mtime,
            })
        })
        .collect()
}

fn render_note_document(
    context: &RenderContext,
    note: &RenderedNote,
    previous: Option<&RenderedNote>,
    next: Option<&RenderedNote>,
    profile: &ResolvedSiteProfile,
    route_urls: &HashMap<String, String>,
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
    folder_index: &BTreeMap<String, Vec<&RenderedNote>>,
    home_note: Option<&RenderedNote>,
) -> String {
    let breadcrumbs = render_breadcrumbs(&note.breadcrumbs);
    let toc = render_toc(&note.headings);
    let backlinks = if profile.backlinks {
        render_note_links("Backlinks", &note.backlinks, route_urls)
    } else {
        String::new()
    };
    let outgoing = render_note_links("Outgoing links", &note.outgoing_links, route_urls);
    let diagnostics = render_note_diagnostics(&note.diagnostics);
    let prev_next = render_prev_next(previous, next);
    let tags = render_note_tags(&note.tags);
    let body = format!(
        concat!(
            "<article class=\"site-main\">{}<div class=\"site-meta\">Updated from {}</div>",
            "{}{}{}{}{}{}{}{}",
            "</article>"
        ),
        breadcrumbs,
        escape_html(&note.source_path),
        tags,
        note.html,
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
        render_folder_summary(note, folder_index),
        render_related_tags(note, route_urls, tag_index),
    );
    let canonical_url = note
        .canonical_url
        .as_deref()
        .and_then(|value| normalize_site_metadata_url(context.base_url.as_deref(), value))
        .or_else(|| canonical_url_for_path(context.base_url.as_deref(), &note.route.url_path));
    let summary_image = note
        .summary_image
        .as_deref()
        .and_then(summary_image_meta_url)
        .and_then(|value| normalize_site_metadata_url(context.base_url.as_deref(), &value));
    render_document_shell(
        context,
        &note.title,
        &note.description,
        &body,
        &[toc, backlinks, outgoing],
        profile,
        false,
        canonical_url.as_deref(),
        summary_image.as_deref(),
    )
}

fn render_home_page(
    context: &RenderContext,
    note: &RenderedNote,
    profile: &ResolvedSiteProfile,
    route_urls: &HashMap<String, String>,
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
    folder_index: &BTreeMap<String, Vec<&RenderedNote>>,
) -> String {
    let body = format!(
        "<article class=\"site-main\">{}{}{}{}</article>",
        render_note_tags(&note.tags),
        note.html,
        render_folder_summary(note, folder_index),
        render_related_tags(note, route_urls, tag_index),
    );
    let canonical_url = note
        .canonical_url
        .as_deref()
        .and_then(|value| normalize_site_metadata_url(context.base_url.as_deref(), value))
        .or_else(|| canonical_url_for_path(context.base_url.as_deref(), "/"));
    let summary_image = note
        .summary_image
        .as_deref()
        .and_then(summary_image_meta_url)
        .and_then(|value| normalize_site_metadata_url(context.base_url.as_deref(), &value));
    render_document_shell(
        context,
        &context.site_title,
        &note.description,
        &body,
        &[render_toc(&note.headings)],
        profile,
        false,
        canonical_url.as_deref(),
        summary_image.as_deref(),
    )
}

fn render_recent_page(
    context: &RenderContext,
    notes: &[RenderedNote],
    profile: &ResolvedSiteProfile,
) -> String {
    let mut notes = notes.iter().collect::<Vec<_>>();
    notes.sort_by(|left, right| {
        right
            .file_mtime
            .cmp(&left.file_mtime)
            .then(left.title.cmp(&right.title))
    });
    let cards = notes
        .into_iter()
        .map(|note| render_card(&note.title, &note.route.url_path, &note.excerpt))
        .collect::<String>();
    render_generic_page(
        context,
        "Recent notes",
        "Most recently updated published notes.",
        &format!(
            "<section class=\"site-listing\"><div class=\"site-card-grid\">{cards}</div></section>"
        ),
        profile,
        false,
        "/recent/",
    )
}

fn render_folder_pages(
    context: &RenderContext,
    folder_index: &BTreeMap<String, Vec<&RenderedNote>>,
    profile: &ResolvedSiteProfile,
) -> Vec<(String, String)> {
    let mut pages = Vec::new();
    let overview = folder_index
        .iter()
        .map(|(folder, notes)| {
            render_card(
                folder,
                &folder_page_href(folder),
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
            &format!("<section class=\"site-listing\"><div class=\"site-card-grid\">{overview}</div></section>"),
            profile,
            false,
            "/folders/",
        ),
    ));
    for (folder, notes) in folder_index {
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
                &format!("<section class=\"site-listing\"><div class=\"site-card-grid\">{list}</div></section>"),
                profile,
                false,
                &folder_page_href(folder),
            ),
        ));
    }
    pages
}

fn render_tag_pages(
    context: &RenderContext,
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
    profile: &ResolvedSiteProfile,
) -> Vec<(String, String)> {
    let mut pages = Vec::new();
    let overview = tag_index
        .iter()
        .map(|(tag, notes)| {
            render_card(
                &format!("#{tag}"),
                &tag_page_href(tag),
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
            &format!("<section class=\"site-listing\"><div class=\"site-card-grid\">{overview}</div></section>"),
            profile,
            false,
            "/tags/",
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
                &format!("<section class=\"site-listing\"><div class=\"site-card-grid\">{list}</div></section>"),
                profile,
                false,
                &tag_page_href(tag),
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
    profile: &ResolvedSiteProfile,
    search_page: bool,
    canonical_path: &str,
) -> String {
    let canonical_url = canonical_url_for_path(context.base_url.as_deref(), canonical_path);
    render_document_shell(
        context,
        title,
        description,
        body,
        &[],
        profile,
        search_page,
        canonical_url.as_deref(),
        None,
    )
}

fn render_document_shell(
    context: &RenderContext,
    title: &str,
    description: &str,
    body: &str,
    sidebar_sections: &[String],
    profile: &ResolvedSiteProfile,
    search_page: bool,
    canonical_url: Option<&str>,
    summary_image_url: Option<&str>,
) -> String {
    let sidebar = sidebar_sections
        .iter()
        .filter(|section| !section.is_empty())
        .cloned()
        .collect::<String>();
    let document_title = render_page_title(profile, context, title);
    let head_assets = render_head_assets(profile);
    let nav = render_top_nav(profile);
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
        "<link rel=\"alternate\" type=\"application/rss+xml\" href=\"/rss.xml\" title=\"RSS\" />"
    } else {
        ""
    };
    let logo = render_logo(profile);
    format!(
        concat!(
            "<!doctype html><html lang=\"{}\"><head><meta charset=\"utf-8\" />",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />",
            "<title>{}</title><meta name=\"description\" content=\"{}\" />{}{}{}",
            "<meta property=\"og:title\" content=\"{}\" />",
            "<meta property=\"og:description\" content=\"{}\" />{}",
            "<link rel=\"stylesheet\" href=\"/assets/vulcan-site.css\" />{}",
            "<script defer src=\"/assets/vulcan-site.js\"></script></head>",
            "<body data-search-asset=\"{}\"><div class=\"site-shell\">",
            "<header class=\"site-header\"><div class=\"site-brand\">{}<div><h1>{}</h1><p>{}</p></div></div><div class=\"site-toolbar\">{}<button data-theme-toggle type=\"button\">Theme</button></div></header>",
            "<div class=\"site-layout\">{}<aside class=\"site-sidebar\">{}</aside></div>",
            "<footer class=\"site-footer\">Built by Vulcan static site builder.</footer></div></body></html>"
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
        head_assets,
        if search_page { "/assets/search-index.json" } else { "" },
        logo,
        escape_html(&context.site_title),
        escape_html(description),
        nav,
        body,
        sidebar,
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

fn render_head_assets(profile: &ResolvedSiteProfile) -> String {
    let mut rendered = String::new();
    if let Some(favicon) = profile.favicon.as_ref() {
        write!(
            rendered,
            "<link rel=\"icon\" href=\"/assets/{}\" />",
            encode_url_path(normalize_relative_path(favicon))
        )
        .expect("writing to string cannot fail");
    }
    for asset in &profile.extra_css {
        write!(
            rendered,
            "<link rel=\"stylesheet\" href=\"/assets/{}\" />",
            encode_url_path(normalize_relative_path(asset))
        )
        .expect("writing to string cannot fail");
    }
    for asset in &profile.extra_js {
        write!(
            rendered,
            "<script defer src=\"/assets/{}\"></script>",
            encode_url_path(normalize_relative_path(asset))
        )
        .expect("writing to string cannot fail");
    }
    rendered
}

fn render_logo(profile: &ResolvedSiteProfile) -> String {
    profile.logo.as_ref().map_or_else(String::new, |logo| {
        format!(
            "<img class=\"site-brand-mark\" src=\"/assets/{}\" alt=\"\" />",
            encode_url_path(normalize_relative_path(logo))
        )
    })
}

fn render_top_nav(profile: &ResolvedSiteProfile) -> String {
    let mut items = vec![
        ("Home", "/".to_string()),
        ("Recent", "/recent/".to_string()),
        ("Folders", "/folders/".to_string()),
        ("Tags", "/tags/".to_string()),
    ];
    if profile.search {
        items.push(("Search", "/search/".to_string()));
    }
    if profile.graph {
        items.push(("Graph", "/graph/".to_string()));
    }
    items
        .into_iter()
        .map(|(label, href)| format!("<a href=\"{href}\">{}</a>", escape_html(label)))
        .collect::<String>()
}

fn render_listing_cards(notes: &[RenderedNote]) -> String {
    let cards = notes
        .iter()
        .map(|note| render_card(&note.title, &note.route.url_path, &note.excerpt))
        .collect::<String>();
    format!("<section class=\"site-listing\"><div class=\"site-card-grid\">{cards}</div></section>")
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
    format!("<nav class=\"site-breadcrumbs\">{parts}</nav>")
}

fn render_toc(headings: &[HtmlRenderHeading]) -> String {
    if headings.is_empty() {
        return String::new();
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
    format!("<section><h2>Contents</h2><ul>{items}</ul></section>")
}

fn render_note_links(
    title: &str,
    note_paths: &[String],
    route_urls: &HashMap<String, String>,
) -> String {
    if note_paths.is_empty() {
        return String::new();
    }
    let items = note_paths
        .iter()
        .map(|path| {
            let href = route_urls.get(path).cloned().unwrap_or_else(|| {
                format!("/notes/{}/", slugify_path(trim_markdown_extension(path)))
            });
            format!(
                "<li><a href=\"{}\">{}</a></li>",
                escape_html(&href),
                escape_html(path)
            )
        })
        .collect::<String>();
    format!(
        "<section><h2>{}</h2><ul>{items}</ul></section>",
        escape_html(title)
    )
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
    format!("<nav class=\"site-inline-nav\">{previous_link}{next_link}</nav>")
}

fn render_note_tags(tags: &[String]) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let items = tags
        .iter()
        .map(|tag| {
            let normalized = normalize_tag(tag);
            format!(
                "<li><a href=\"{}\">#{}</a></li>",
                tag_page_href(&normalized),
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
        folder_page_href(&folder),
        count
    )
}

fn render_related_tags(
    note: &RenderedNote,
    route_urls: &HashMap<String, String>,
    tag_index: &BTreeMap<String, Vec<&RenderedNote>>,
) -> String {
    let mut related = BTreeSet::<String>::new();
    for tag in &note.tags {
        let normalized = normalize_tag(tag);
        if let Some(notes) = tag_index.get(&normalized) {
            for related_note in notes {
                if related_note.source_path != note.source_path {
                    related.insert(related_note.source_path.clone());
                }
            }
        }
    }
    if related.is_empty() {
        return String::new();
    }
    let items = related
        .into_iter()
        .take(6)
        .map(|path| {
            let href = route_urls.get(&path).cloned().unwrap_or_else(|| {
                format!("/notes/{}/", slugify_path(trim_markdown_extension(&path)))
            });
            format!(
                "<li><a href=\"{}\">{}</a></li>",
                escape_html(&href),
                escape_html(&path)
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

fn build_search_index(
    paths: &VaultPaths,
    notes: &[ExportedNoteDocument],
    routes_by_path: &HashMap<String, SiteRoute>,
) -> Result<Value, AppError> {
    let published = notes
        .iter()
        .map(|note| note.note.document_path.as_str())
        .collect::<HashSet<_>>();
    let static_index = export_static_search_index(paths).map_err(AppError::operation)?;
    let entries = static_index
        .entries
        .into_iter()
        .filter(|entry| published.contains(entry.document_path.as_str()))
        .filter_map(|entry| {
            let route = routes_by_path.get(&entry.document_path)?;
            let title = routes_by_path.get(&entry.document_path).map_or_else(
                || trim_markdown_extension(&entry.document_path).to_string(),
                |site_route| site_route.title.clone(),
            );
            let excerpt = excerpt_from_markdown(&entry.content);
            let tags = notes
                .iter()
                .find(|note| note.note.document_path == entry.document_path)
                .map_or_else(Vec::new, |note| note.note.tags.clone());
            Some(SearchIndexEntry {
                title,
                url: route.url_path.clone(),
                excerpt,
                content: entry.content,
                tags,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "version": 1, "entries": entries }))
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
    let graph = export_graph(paths).map_err(AppError::operation)?;
    let nodes = graph
        .nodes
        .into_iter()
        .filter(|node| published.contains(node.path.as_str()))
        .map(|node| {
            serde_json::json!({
                "id": node.id,
                "path": node.path,
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
        escape_xml(context.base_url.as_deref().unwrap_or("/")),
        escape_xml(&context.site_title),
    )
}

fn build_sitemap(base_url: &str, files: &BTreeSet<String>, notes: &[RenderedNote]) -> String {
    let note_urls = notes
        .iter()
        .map(|note| note.route.url_path.as_str())
        .chain(files.iter().filter_map(|path| {
            if path.ends_with("index.html") {
                Some(path.trim_end_matches("index.html"))
            } else if Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
            {
                Some(path.as_str())
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
                escape_xml(&format!(
                    "{}{}",
                    base_url.trim_end_matches('/'),
                    if path.starts_with('/') {
                        path.to_string()
                    } else {
                        format!("/{path}")
                    }
                ))
            )
        })
        .collect::<String>();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{body}</urlset>")
}

fn links_by_source(links: &[ExportLinkRecord]) -> HashMap<String, Vec<ExportLinkRecord>> {
    let mut grouped = HashMap::<String, Vec<ExportLinkRecord>>::new();
    for link in links {
        grouped
            .entry(link.source_document_path.clone())
            .or_default()
            .push(link.clone());
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

fn build_tag_href_map(notes: &[ExportedNoteDocument]) -> HashMap<String, String> {
    notes
        .iter()
        .flat_map(|note| note.note.tags.iter())
        .map(|tag| normalize_tag(tag))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|tag| (tag.clone(), tag_page_href(&tag)))
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

fn collect_asset_links(links: &[ExportLinkRecord]) -> HashMap<String, String> {
    links
        .iter()
        .filter_map(|link| {
            if !is_markdown_asset(link) {
                return None;
            }
            let target = link.resolved_target_path.as_ref()?;
            Some((
                target.clone(),
                format!("/assets/{}", encode_url_path(target)),
            ))
        })
        .collect()
}

fn apply_link_policy_to_source(
    source: &str,
    links: &[ExportLinkRecord],
    published_paths: &HashSet<String>,
    policy: SiteLinkPolicyConfig,
) -> String {
    if !matches!(
        policy,
        SiteLinkPolicyConfig::DropLink | SiteLinkPolicyConfig::RenderPlainText
    ) {
        return source.to_string();
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
                    .map_or(true, |path| !published_paths.contains(path));
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
    replacements.sort_by_key(|(start, _, _)| *start);
    replacements.reverse();
    let mut rendered = source.to_string();
    for (start, end, replacement) in replacements {
        if start <= end && end <= rendered.len() {
            rendered.replace_range(start..end, &replacement);
        }
    }
    rendered
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

fn summary_image_meta_url(value: &str) -> Option<String> {
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
        Some(format!("/assets/{}", encode_url_path(&relative)))
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

fn folder_page_href(folder: &str) -> String {
    if folder.is_empty() {
        "/folders/".to_string()
    } else {
        format!("/folders/{}/", slugify_path(folder))
    }
}

fn tag_page_href(tag: &str) -> String {
    format!("/tags/{}/", slugify_segment(tag))
}

fn asset_output_path(output_dir: &Path, asset_path: &str) -> PathBuf {
    output_dir
        .join("assets")
        .join(normalize_relative_path(asset_path))
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

fn copy_asset(paths: &VaultPaths, relative: &str, destination: &Path) -> Result<(), AppError> {
    copy_file_from_vault(paths, Path::new(relative), destination)
}

fn copy_file_from_vault(
    paths: &VaultPaths,
    relative: &Path,
    destination: &Path,
) -> Result<(), AppError> {
    let source = paths.vault_root().join(relative);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    fs::copy(source, destination).map_err(AppError::operation)?;
    Ok(())
}

fn write_output_file(path: &Path, contents: &str) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    fs::write(path, contents).map_err(AppError::operation)?;
    Ok(())
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
mod tests {
    use super::*;
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
        fs::write(vault_root.join("site.css"), "body { outline: none; }")
            .expect("css should write");
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
        assert!(vault_root.join(".vulcan/site/public/rss.xml").exists());
        let home_html = fs::read_to_string(vault_root.join(".vulcan/site/public/index.html"))
            .expect("home page should read");
        assert!(home_html.contains("Published Garden"));
        let search_html =
            fs::read_to_string(vault_root.join(".vulcan/site/public/search/index.html"))
                .expect("search page should read");
        assert!(search_html.contains("site-search-input"));
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
        fs::write(vault_root.join("site/social.png"), b"social")
            .expect("social image should write");
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
        let search_html =
            fs::read_to_string(vault_root.join(".vulcan/site/public/search/index.html"))
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
}
