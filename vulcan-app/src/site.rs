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
    SiteRawHtmlPolicyConfig,
};
use vulcan_core::graph::resolve_note_reference;
use vulcan_core::html::{
    render_vault_html, HtmlDataviewJsPolicy, HtmlLinkTargets, HtmlRawHtmlPolicy,
    HtmlRenderDiagnostic, HtmlRenderHeading, HtmlRenderOptions,
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
    ".site-skip-link { position: absolute; left: 1rem; top: 0.75rem; padding: 0.65rem 0.9rem; border-radius: 999px; background: var(--accent); color: #fff; text-decoration: none; transform: translateY(-180%); transition: transform 0.18s ease; z-index: 10000; }\n",
    ".site-skip-link:focus { transform: translateY(0); }\n",
    ".site-header { display: flex; gap: 1rem; justify-content: space-between; align-items: center; margin-bottom: 2rem; }\n",
    ".site-brand { display: flex; gap: 0.85rem; align-items: center; }\n",
    ".site-brand-title { margin: 0; font-size: clamp(2rem, 4vw, 3rem); font-weight: 700; }\n",
    ".site-brand-title a { color: inherit; text-decoration: none; }\n",
    ".site-brand p { margin: 0.4rem 0 0; color: var(--muted); }\n",
    ".site-brand-mark { width: 3rem; height: 3rem; border-radius: 0.9rem; object-fit: cover; border: 1px solid var(--border); box-shadow: var(--shadow); background: rgba(255,255,255,0.35); }\n",
    ".site-toolbar { display: flex; gap: 0.75rem; align-items: center; flex-wrap: wrap; }\n",
    ".site-top-nav { display: flex; gap: 0.75rem; align-items: center; flex-wrap: wrap; }\n",
    ".site-toolbar a, .site-toolbar button { border: 1px solid var(--border); background: var(--panel); color: var(--text); border-radius: 999px; padding: 0.55rem 0.9rem; text-decoration: none; cursor: pointer; box-shadow: var(--shadow); }\n",
    ".site-layout { display: grid; grid-template-columns: minmax(0, 1fr) 280px; gap: 1.5rem; }\n",
    ".site-main, .site-sidebar > section, .site-listing, .site-search-card, .site-graph-card { background: var(--panel); border: 1px solid var(--border); border-radius: 1.5rem; box-shadow: var(--shadow); }\n",
    ".site-main { padding: 1.5rem; }\n",
    ".site-sidebar { display: grid; gap: 1rem; align-content: start; }\n",
    ".site-sidebar > section { padding: 1rem 1.1rem; }\n",
    ".site-meta, .site-breadcrumbs, .site-footer, .site-listing p, .site-empty { color: var(--muted); }\n",
    ".site-breadcrumbs { display: flex; gap: 0.5rem; flex-wrap: wrap; font-size: 0.95rem; margin-bottom: 1rem; }\n",
    ".site-listing, .site-search-card, .site-graph-card { padding: 1.25rem; }\n",
    ".site-listing ul, .site-sidebar ul { padding-left: 1.2rem; margin: 0.75rem 0 0; }\n",
    ".site-card-grid { display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); }\n",
    ".site-card { border: 1px solid var(--border); border-radius: 1.2rem; padding: 1rem; background: rgba(255,255,255,0.22); }\n",
    ".site-footer { margin-top: 2rem; font-size: 0.95rem; }\n",
    ".site-search-input { width: 100%; padding: 0.8rem 1rem; border-radius: 999px; border: 1px solid var(--border); background: rgba(255,255,255,0.55); color: var(--text); }\n",
    ".site-search-launch, .site-search-dialog-header button { border: 1px solid var(--border); background: var(--panel); color: var(--text); border-radius: 999px; padding: 0.55rem 0.9rem; text-decoration: none; cursor: pointer; box-shadow: var(--shadow); }\n",
    ".site-search-dialog[hidden] { display: none; }\n",
    ".site-search-dialog { position: fixed; inset: 0; z-index: 9998; padding: 1.25rem; background: rgba(19, 17, 15, 0.56); backdrop-filter: blur(10px); }\n",
    ".site-search-dialog-panel { max-width: 44rem; margin: 2rem auto 0; background: var(--panel); border: 1px solid var(--border); border-radius: 1.5rem; box-shadow: var(--shadow); padding: 1.1rem; }\n",
    ".site-search-dialog-header { display: flex; gap: 1rem; align-items: center; justify-content: space-between; }\n",
    ".site-search-results { list-style: none; padding: 0; margin: 1rem 0 0; display: grid; gap: 0.8rem; }\n",
    ".site-search-results li { border: 1px solid var(--border); border-radius: 1rem; padding: 0.9rem 1rem; background: rgba(255,255,255,0.18); }\n",
    ".site-search-results a { font-weight: 700; text-decoration: none; }\n",
    ".site-search-results mark { background: rgba(13, 107, 87, 0.2); color: inherit; padding: 0 0.15em; border-radius: 0.2rem; }\n",
    ".site-local-graph-list { list-style: none; padding: 0; margin: 0.75rem 0 0; display: grid; gap: 0.75rem; }\n",
    ".site-local-graph-list li { border-bottom: 1px solid var(--border); padding-bottom: 0.75rem; }\n",
    ".site-local-graph-list li:last-child { border-bottom: 0; padding-bottom: 0; }\n",
    ".site-local-graph-direction { display: block; font-size: 0.8rem; letter-spacing: 0.05em; text-transform: uppercase; color: var(--muted); margin-bottom: 0.2rem; }\n",
    ".site-visually-hidden { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }\n",
    ".site-pill-list { display: flex; gap: 0.5rem; flex-wrap: wrap; list-style: none; padding: 0; margin: 0.9rem 0 0; }\n",
    ".site-pill-list a { display: inline-flex; border: 1px solid var(--border); padding: 0.35rem 0.75rem; border-radius: 999px; text-decoration: none; }\n",
    ".site-inline-nav { display: flex; gap: 0.8rem; justify-content: space-between; margin-top: 2rem; flex-wrap: wrap; }\n",
    ".site-callout { border-left: 4px solid var(--accent); padding-left: 1rem; color: var(--muted); }\n",
    ".site-diagnostics { margin-top: 1rem; border-radius: 1rem; border: 1px solid rgba(178, 69, 54, 0.35); background: rgba(178, 69, 54, 0.08); padding: 1rem; }\n",
    ".site-live-overlay { position: fixed; right: 1rem; bottom: 1rem; z-index: 9999; background: rgba(20, 20, 20, 0.92); color: #fff; padding: 0.9rem 1rem; border-radius: 1rem; max-width: 24rem; box-shadow: 0 18px 36px rgba(0,0,0,0.35); }\n",
    "a:focus-visible, button:focus-visible, input:focus-visible { outline: 2px solid var(--accent-strong); outline-offset: 3px; }\n",
    "@media (max-width: 900px) { .site-layout { grid-template-columns: 1fr; } .site-header { flex-direction: column; align-items: start; } .site-search-dialog { padding: 0.65rem; } .site-search-dialog-panel { margin-top: 0; min-height: calc(100vh - 1.3rem); border-radius: 1.2rem; } }\n"
);

const DEFAULT_THEME_JS: &str = r#"(() => {
  const root = document.documentElement;
  const body = document.body;
  const syncThemeButtons = () => {
    const pressed = root.dataset.theme === 'dark' ? 'true' : 'false';
    document
      .querySelectorAll('[data-theme-toggle]')
      .forEach((button) => button.setAttribute('aria-pressed', pressed));
  };
  const storedTheme = localStorage.getItem('vulcan-site-theme');
  if (storedTheme) root.dataset.theme = storedTheme;
  syncThemeButtons();
  document.addEventListener('click', (event) => {
    const button = event.target.closest('[data-theme-toggle]');
    if (!button) return;
    const next = root.dataset.theme === 'dark' ? 'light' : 'dark';
    root.dataset.theme = next;
    localStorage.setItem('vulcan-site-theme', next);
    syncThemeButtons();
  });

  const escapeHtml = (value) =>
    String(value).replace(/[&<>"']/g, (char) => ({
      '&': '&amp;',
      '<': '&lt;',
      '>': '&gt;',
      '"': '&quot;',
      "'": '&#39;',
    })[char] ?? char);
  const escapeRegExp = (value) => String(value).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const highlightText = (value, query) => {
    const terms = query.trim().split(/\s+/).filter(Boolean).map(escapeRegExp);
    if (!terms.length) return escapeHtml(value);
    const regex = new RegExp(`(${terms.join('|')})`, 'ig');
    return escapeHtml(value).replace(regex, '<mark>$1</mark>');
  };

  const searchDialog = document.querySelector('[data-site-search-dialog]');
  const searchInput = searchDialog?.querySelector('[data-site-search-input]');
  const searchResults = searchDialog?.querySelector('[data-site-search-results]');
  const searchAsset = body.dataset.searchAsset;
  const openSearch = () => {
    if (!searchDialog || !searchInput) return;
    searchDialog.hidden = false;
    searchInput.focus();
    searchInput.select();
  };
  const closeSearch = () => {
    if (!searchDialog) return;
    searchDialog.hidden = true;
  };
  if (searchDialog) {
    document.querySelectorAll('[data-site-search-open]').forEach((button) => {
      button.addEventListener('click', openSearch);
    });
    document.querySelectorAll('[data-site-search-close]').forEach((button) => {
      button.addEventListener('click', closeSearch);
    });
    searchDialog.addEventListener('click', (event) => {
      if (event.target === searchDialog) closeSearch();
    });
  }
  if (searchInput && searchResults && searchAsset) {
    let entries = [];
    fetch(searchAsset)
      .then((response) => (response.ok ? response.json() : null))
      .then((payload) => {
        entries = payload?.entries || [];
      })
      .catch(() => {});
    const renderSearchResults = () => {
      const query = searchInput.value.trim().toLowerCase();
      if (!query) {
        searchResults.innerHTML = '<li>Type to search the published site.</li>';
        return;
      }
      const hits = entries
        .filter((entry) =>
          `${entry.title} ${entry.content} ${entry.tags.join(' ')}`.toLowerCase().includes(query)
        )
        .slice(0, 20);
      if (!hits.length) {
        searchResults.innerHTML = '<li>No matching notes in the published subset.</li>';
        return;
      }
      searchResults.innerHTML = hits
        .map(
          (hit) => `<li><a href="${hit.url}">${highlightText(hit.title, query)}</a><div>${highlightText(hit.excerpt, query)}</div></li>`
        )
        .join('');
    };
    searchInput.addEventListener('input', renderSearchResults);
    renderSearchResults();
    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && searchDialog && !searchDialog.hidden) {
        event.preventDefault();
        closeSearch();
        return;
      }
      if (event.key === '/' && !event.metaKey && !event.ctrlKey && !event.altKey) {
        if (document.activeElement && /input|textarea/i.test(document.activeElement.tagName)) return;
        event.preventDefault();
        openSearch();
      }
    });
  }

  const graphAsset = body.dataset.graphAsset;
  const localGraphCards = [...document.querySelectorAll('[data-site-local-graph]')];
  if (graphAsset && localGraphCards.length) {
    fetch(graphAsset)
      .then((response) => (response.ok ? response.json() : null))
      .then((payload) => {
        const nodes = payload?.nodes || [];
        const edges = payload?.edges || [];
        const nodeByPath = new Map(nodes.map((node) => [node.path, node]));
        for (const card of localGraphCards) {
          const notePath = card.getAttribute('data-site-note-path') || body.dataset.currentNotePath;
          const list = card.querySelector('[data-site-local-graph-list]');
          if (!notePath || !list) continue;
          const neighbors = [];
          for (const edge of edges) {
            if (edge.source === notePath && nodeByPath.has(edge.target)) {
              neighbors.push({ direction: 'Outgoing', node: nodeByPath.get(edge.target) });
            } else if (edge.target === notePath && nodeByPath.has(edge.source)) {
              neighbors.push({ direction: 'Backlink', node: nodeByPath.get(edge.source) });
            }
          }
          if (!neighbors.length) {
            list.innerHTML = '<li>No published neighbors for this note.</li>';
            continue;
          }
          const unique = new Map();
          for (const neighbor of neighbors) {
            unique.set(`${neighbor.direction}:${neighbor.node.path}`, neighbor);
          }
          list.innerHTML = [...unique.values()]
            .slice(0, 10)
            .map(
              ({ direction, node }) =>
                `<li><span class="site-local-graph-direction">${direction}</span><a href="${node.url}">${escapeHtml(node.title || node.path)}</a></li>`
            )
            .join('');
        }
      })
      .catch(() => {});
  }

  let liveVersion = null;
  const liveUrl = body.dataset.liveReloadUrl || '/__vulcan_site/live-reload.json';
  const liveSseUrl = body.dataset.liveReloadSseUrl || '';
  const overlayId = 'vulcan-site-live-overlay';
  const ensureOverlay = (message) => {
    let overlay = document.getElementById(overlayId);
    if (!overlay) {
      overlay = document.createElement('div');
      overlay.id = overlayId;
      overlay.className = 'site-live-overlay';
      document.body.appendChild(overlay);
    }
    overlay.innerHTML = message;
  };
  const clearOverlay = () => {
    const overlay = document.getElementById(overlayId);
    if (overlay) overlay.remove();
  };
  const formatDiagnosticsOverlay = (payload) => {
    if (payload.last_error) return escapeHtml(payload.last_error);
    if (!Array.isArray(payload.diagnostics) || !payload.diagnostics.length) return '';
    return payload.diagnostics
      .slice(0, 3)
      .map((diagnostic) => {
        const path = diagnostic.source_path ? ` <br><small>${escapeHtml(diagnostic.source_path)}</small>` : '';
        return `<strong>[${escapeHtml(diagnostic.level)}]</strong> ${escapeHtml(diagnostic.kind)} ${escapeHtml(diagnostic.message)}${path}`;
      })
      .join('<hr>');
  };
  const handleLivePayload = (payload) => {
    if (!payload) return;
    if (liveVersion === null) {
      liveVersion = payload.version;
    } else if (payload.version !== liveVersion) {
      window.location.reload();
      return;
    }
    const overlayMessage = formatDiagnosticsOverlay(payload);
    if (overlayMessage) ensureOverlay(overlayMessage);
    else clearOverlay();
  };
  const startPolling = () => {
    window.setInterval(() => {
      fetch(liveUrl, { cache: 'no-store' })
        .then((response) => (response.ok ? response.json() : null))
        .then(handleLivePayload)
        .catch(() => {});
    }, 1200);
  };
  if (window.EventSource && liveSseUrl) {
    const source = new EventSource(liveSseUrl);
    source.addEventListener('update', (event) => {
      try {
        handleLivePayload(JSON.parse(event.data));
      } catch (_) {}
    });
    source.onerror = () => {
      source.close();
      startPolling();
    };
  } else {
    startPolling();
  }
})();"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderContext {
    pub profile: String,
    pub site_title: String,
    pub language: String,
    pub theme: String,
    pub base_url: Option<String>,
    pub deploy_path: String,
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
    pub deploy_path: String,
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
    pub deploy_path: String,
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
    pub changed_files: Vec<String>,
    pub deleted_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontendBundleRequest {
    pub profile: Option<String>,
    pub output_dir: PathBuf,
    pub clean: bool,
    pub dry_run: bool,
    pub pretty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontendBundleContractInfo {
    pub name: String,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontendBundleProfile {
    pub name: String,
    pub title: String,
    pub deploy_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub language: String,
    pub theme: String,
    pub search: bool,
    pub graph: bool,
    pub backlinks: bool,
    pub rss: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontendBundleArtifactPaths {
    pub route_manifest: String,
    pub hover_previews: String,
    pub recent_notes: String,
    pub related_notes: String,
    pub note_index: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_index: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    pub invalidation: String,
    pub schema: String,
    pub typescript: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub copied_assets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrontendBundleNoteIndexEntry {
    pub source_path: String,
    pub title: String,
    pub excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_image: Option<String>,
    pub route: SiteRoute,
    pub document_path: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrontendBundleNoteDocument {
    pub source_path: String,
    pub title: String,
    pub excerpt: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_image: Option<String>,
    pub route: SiteRoute,
    pub body_html: String,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrontendBundleContract {
    pub contract: FrontendBundleContractInfo,
    pub profile: FrontendBundleProfile,
    pub context: RenderContext,
    pub note_count: usize,
    pub diagnostics: Vec<SiteDiagnostic>,
    pub routes: Vec<SiteRoute>,
    pub notes: Vec<FrontendBundleNoteIndexEntry>,
    pub artifacts: FrontendBundleArtifactPaths,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontendBundleInvalidationReport {
    pub changed_files: Vec<String>,
    pub deleted_files: Vec<String>,
    pub changed_routes: Vec<String>,
    pub deleted_routes: Vec<String>,
    pub changed_assets: Vec<String>,
    pub deleted_assets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrontendBundleBuildReport {
    pub profile: String,
    pub output_dir: String,
    pub deploy_path: String,
    pub dry_run: bool,
    pub clean: bool,
    pub note_count: usize,
    pub asset_count: usize,
    pub diagnostics: Vec<SiteDiagnostic>,
    pub routes: Vec<SiteRoute>,
    pub note_documents: Vec<FrontendBundleNoteDocument>,
    pub contract: FrontendBundleContract,
    pub invalidation: FrontendBundleInvalidationReport,
    pub files: Vec<String>,
    pub changed_files: Vec<String>,
    pub deleted_files: Vec<String>,
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
    deploy_path: String,
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
    raw_html: SiteRawHtmlPolicyConfig,
    theme_overrides: ResolvedSiteTheme,
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

#[derive(Debug, Clone, Default)]
struct ResolvedSiteTheme {
    css_assets: Vec<PathBuf>,
    js_assets: Vec<PathBuf>,
    head: Option<String>,
    header: Option<String>,
    nav: Option<String>,
    footer: Option<String>,
    note_before: Option<String>,
    note_after: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SearchIndexEntry {
    title: String,
    url: String,
    excerpt: String,
    content: String,
    tags: Vec<String>,
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
            let note_count = select_site_notes(paths, &profile)?.len();
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
    let mut changed_files = BTreeSet::<String>::new();
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
        deploy_path: plan.profile.deploy_path.clone(),
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
            if write_output_file(&path, &html)? {
                changed_files.insert(note.route.output_path.clone());
            }
        }
        files.insert(note.route.output_path.clone());
    }

    let asset_links = collect_asset_links(&plan.links, &plan.profile.deploy_path);
    for (source_path, href) in &asset_links {
        let destination = asset_output_path(&output_dir, source_path);
        if !request.dry_run && copy_asset(paths, source_path, &destination)? {
            changed_files.insert(display_path(
                destination
                    .strip_prefix(&output_dir)
                    .unwrap_or(&destination),
            ));
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
        if !request.dry_run && copy_file_from_vault(paths, summary_image, &destination)? {
            changed_files.insert(display_path(
                destination
                    .strip_prefix(&output_dir)
                    .unwrap_or(&destination),
            ));
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
        if !request.dry_run && copy_file_from_vault(paths, extra_asset, &destination)? {
            changed_files.insert(display_path(
                destination
                    .strip_prefix(&output_dir)
                    .unwrap_or(&destination),
            ));
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
            if !request.dry_run && copy_file_from_vault(paths, &asset, &destination)? {
                changed_files.insert(display_path(
                    destination
                        .strip_prefix(&output_dir)
                        .unwrap_or(&destination),
                ));
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
        let search_index = build_search_index(paths, &plan.notes, &routes_by_path)?;
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
                        "<p class=\"site-meta\">Use the toolbar search button or press / anywhere in the site. This page keeps the same search UI available as a full-screen mobile sheet.</p>",
                        "<button class=\"site-search-launch\" type=\"button\" data-site-search-open>Open search</button>",
                        "</section>"
                    ),
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
        let graph_json = build_graph_asset(paths, &rendered_notes)?;
        let graph_path = output_dir.join("assets/graph.json");
        if !request.dry_run {
            if write_output_file(&graph_path, &graph_json)? {
                changed_files.insert("assets/graph.json".to_string());
            }
            let graph_card = format!(
                concat!(
                    "<section class=\"site-graph-card\"><h2>Published graph</h2>",
                    "<p>This build includes a filtered graph asset reused by later WebUI/wiki work.</p>",
                    "<pre><code>{}</code></pre></section>"
                ),
                escape_html(&graph_json)
            );
            if write_output_file(
                &output_dir.join("graph/index.html"),
                &render_generic_page(
                    &context,
                    "Graph",
                    "Graph export",
                    &graph_card,
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

    let folder_pages = render_folder_pages(&context, &folder_index, &plan.profile);
    for (relative_path, body) in folder_pages {
        if !request.dry_run && write_output_file(&output_dir.join(&relative_path), &body)? {
            changed_files.insert(relative_path.clone());
        }
        files.insert(relative_path);
    }

    let tag_pages = render_tag_pages(&context, &tag_index, &plan.profile);
    for (relative_path, body) in tag_pages {
        if !request.dry_run && write_output_file(&output_dir.join(&relative_path), &body)? {
            changed_files.insert(relative_path.clone());
        }
        files.insert(relative_path);
    }

    let recent_html = render_recent_page(&context, &rendered_notes, &plan.profile);
    if !request.dry_run && write_output_file(&output_dir.join("recent/index.html"), &recent_html)? {
        changed_files.insert("recent/index.html".to_string());
    }
    files.insert("recent/index.html".to_string());

    if let Some(home) = home_note.as_ref() {
        let home_html = render_home_page(&context, home, &plan.profile, &tag_index, &folder_index);
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
    changed_files.extend(deleted_files.iter().cloned());
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
    let mut rendered_notes = render_site_notes(paths, &plan)?;
    rendered_notes.sort_by(|left, right| left.route.url_path.cmp(&right.route.url_path));

    let routes_by_path = rendered_notes
        .iter()
        .map(|note| (note.source_path.clone(), note.route.clone()))
        .collect::<HashMap<_, _>>();
    let context = RenderContext {
        profile: plan.profile.name.clone(),
        site_title: plan.profile.title.clone(),
        language: plan.profile.language.clone(),
        theme: plan.profile.theme.clone(),
        base_url: plan.profile.base_url.clone(),
        deploy_path: plan.profile.deploy_path.clone(),
    };
    let tag_index = build_tag_index(&rendered_notes);
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

    for note in &note_documents {
        let relative_path = frontend_bundle_note_document_path(&note.route);
        let payload = render_json_payload(note, request.pretty)?;
        if !request.dry_run && write_output_file(&output_dir.join(&relative_path), &payload)? {
            changed_files.insert(relative_path.clone());
            changed_routes.insert(note.route.url_path.clone());
        }
        files.insert(relative_path);
    }

    let asset_links = collect_asset_links(&plan.links, &plan.profile.deploy_path);
    for source_path in asset_links.keys() {
        let destination = asset_output_path(&output_dir, source_path);
        let relative_path = display_path(
            destination
                .strip_prefix(&output_dir)
                .unwrap_or(&destination),
        );
        if !request.dry_run && copy_asset(paths, source_path, &destination)? {
            changed_files.insert(relative_path.clone());
        }
        files.insert(relative_path.clone());
        copied_assets.insert(relative_path);
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
        let relative_path = display_path(
            destination
                .strip_prefix(&output_dir)
                .unwrap_or(&destination),
        );
        if !request.dry_run && copy_file_from_vault(paths, summary_image, &destination)? {
            changed_files.insert(relative_path.clone());
        }
        files.insert(relative_path.clone());
        copied_assets.insert(relative_path);
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
        let relative_path = display_path(
            destination
                .strip_prefix(&output_dir)
                .unwrap_or(&destination),
        );
        if !request.dry_run && copy_file_from_vault(paths, extra_asset, &destination)? {
            changed_files.insert(relative_path.clone());
        }
        files.insert(relative_path.clone());
        copied_assets.insert(relative_path);
    }

    for extra_pattern in &plan.profile.asset_policy.include_folders {
        for asset in collect_extra_assets(paths, extra_pattern)? {
            let destination = output_dir
                .join("assets")
                .join(normalize_relative_path(&asset));
            let relative_path = display_path(
                destination
                    .strip_prefix(&output_dir)
                    .unwrap_or(&destination),
            );
            if !request.dry_run && copy_file_from_vault(paths, &asset, &destination)? {
                changed_files.insert(relative_path.clone());
            }
            files.insert(relative_path.clone());
            copied_assets.insert(relative_path);
        }
    }

    let route_manifest_path = "assets/route-manifest.json".to_string();
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
        let search_index = build_search_index(paths, &plan.notes, &routes_by_path)?;
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
        },
        context: context.clone(),
        note_count: note_documents.len(),
        diagnostics: plan.diagnostics.clone(),
        routes: plan.routes.clone(),
        notes: note_index.clone(),
        artifacts: FrontendBundleArtifactPaths {
            route_manifest: route_manifest_path.clone(),
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
        search: raw.search.unwrap_or(true),
        graph: raw.graph.unwrap_or(true),
        backlinks: raw.backlinks.unwrap_or(true),
        rss: raw.rss.unwrap_or(false),
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
    let routes = plan_note_routes(&prepared.notes, &profile.name, &profile.deploy_path);
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

fn plan_note_routes(
    notes: &[ExportedNoteDocument],
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
    let asset_hrefs = collect_asset_links(&plan.links, &plan.profile.deploy_path);
    let note_hrefs = route_map
        .iter()
        .map(|(path, route)| (path.clone(), route.url_path.clone()))
        .collect::<HashMap<_, _>>();
    let tag_hrefs = build_tag_href_map(&plan.notes, &plan.profile.deploy_path);
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
                                collect_asset_links(&plan.links, &plan.profile.deploy_path)
                                    .get(&target_path)
                                    .cloned()
                                    .unwrap_or_default()
                            },
                            |route| route.url_path.clone(),
                        )
                    } else {
                        collect_asset_links(&plan.links, &plan.profile.deploy_path)
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
    let local_graph = if profile.graph {
        render_local_graph_card(note)
    } else {
        String::new()
    };
    let backlinks = if profile.backlinks {
        render_note_links(
            &context.deploy_path,
            "Backlinks",
            &note.backlinks,
            route_urls,
        )
    } else {
        String::new()
    };
    let outgoing = render_note_links(
        &context.deploy_path,
        "Outgoing links",
        &note.outgoing_links,
        route_urls,
    );
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
            "<article class=\"site-main\">{}<div class=\"site-meta\">Updated from {}</div>",
            "{}{}{}{}{}{}{}{}",
            "</article>"
        ),
        breadcrumbs,
        escape_html(&note.source_path),
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
        &[toc, local_graph, backlinks, outgoing],
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
        Some(&note.source_path),
    )
}

fn render_recent_page(
    context: &RenderContext,
    notes: &[RenderedNote],
    profile: &ResolvedSiteProfile,
) -> String {
    let cards = build_recent_manifest(notes)
        .into_iter()
        .map(|note| render_card(&note.title, &note.url, &note.excerpt))
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
        &prefixed_site_path(&context.deploy_path, "/recent/"),
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
            &format!("<section class=\"site-listing\"><div class=\"site-card-grid\">{overview}</div></section>"),
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
                &format!("<section class=\"site-listing\"><div class=\"site-card-grid\">{list}</div></section>"),
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
    profile: &ResolvedSiteProfile,
) -> Vec<(String, String)> {
    let mut pages = Vec::new();
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
            &format!("<section class=\"site-listing\"><div class=\"site-card-grid\">{overview}</div></section>"),
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
                &format!("<section class=\"site-listing\"><div class=\"site-card-grid\">{list}</div></section>"),
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
    sidebar_sections: &[String],
    profile: &ResolvedSiteProfile,
    _search_page: bool,
    canonical_url: Option<&str>,
    summary_image_url: Option<&str>,
    current_note_path: Option<&str>,
) -> String {
    let sidebar = sidebar_sections
        .iter()
        .filter(|section| !section.is_empty())
        .cloned()
        .collect::<String>();
    let document_title = render_page_title(profile, context, title);
    let head_assets = render_head_assets(context, profile);
    let default_nav = render_top_nav(context, profile);
    let search_button = if profile.search {
        "<button type=\"button\" data-site-search-open aria-haspopup=\"dialog\">Search</button>"
    } else {
        ""
    };
    let theme_toggle =
        "<button data-theme-toggle type=\"button\" aria-label=\"Toggle color theme\" aria-pressed=\"false\">Theme</button>";
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
        &logo,
    );
    let head_partial = render_theme_partial(profile.theme_overrides.head.as_deref(), &tokens);
    let header = profile.theme_overrides.header.as_deref().map_or_else(
        || {
            render_default_header(
                context,
                description,
                &nav,
                search_button,
                theme_toggle,
                &logo,
            )
        },
        |partial| render_theme_partial(Some(partial), &tokens),
    );
    let footer = profile
        .theme_overrides
        .footer
        .as_deref()
        .map_or_else(render_default_footer, |partial| {
            render_theme_partial(Some(partial), &tokens)
        });
    format!(
        concat!(
            "<!doctype html><html lang=\"{}\"><head><meta charset=\"utf-8\" />",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />",
            "<title>{}</title><meta name=\"description\" content=\"{}\" />{}{}{}",
            "<meta property=\"og:title\" content=\"{}\" />",
            "<meta property=\"og:description\" content=\"{}\" />{}",
            "<link rel=\"stylesheet\" href=\"{}\" />{}{}",
            "<script defer src=\"{}\"></script></head>",
            "<body data-search-asset=\"{}\" data-graph-asset=\"{}\" data-live-reload-url=\"{}\" data-live-reload-sse-url=\"{}\" data-current-note-path=\"{}\"><a class=\"site-skip-link\" href=\"#main-content\">Skip to content</a>{}<div class=\"site-shell\">",
            "{}<div class=\"site-layout\"><main id=\"main-content\" class=\"site-content\">{}</main><aside class=\"site-sidebar\" aria-label=\"Page context\">{}</aside></div>{}</div></body></html>"
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
        search_dialog,
        header,
        body,
        sidebar,
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
    let mut items = vec![
        ("Home", site_root_href(&context.deploy_path)),
        (
            "Recent",
            prefixed_site_path(&context.deploy_path, "/recent/"),
        ),
        (
            "Folders",
            prefixed_site_path(&context.deploy_path, "/folders/"),
        ),
        ("Tags", prefixed_site_path(&context.deploy_path, "/tags/")),
    ];
    if profile.search {
        items.push((
            "Search",
            prefixed_site_path(&context.deploy_path, "/search/"),
        ));
    }
    if profile.graph {
        items.push(("Graph", prefixed_site_path(&context.deploy_path, "/graph/")));
    }
    items
        .into_iter()
        .map(|(label, href)| format!("<a href=\"{href}\">{}</a>", escape_html(label)))
        .collect::<String>()
}

fn render_default_header(
    context: &RenderContext,
    description: &str,
    nav: &str,
    search_button: &str,
    theme_toggle: &str,
    logo: &str,
) -> String {
    format!(
        concat!(
            "<header class=\"site-header\"><div class=\"site-brand\">{}<div>",
            "<p class=\"site-brand-title\"><a href=\"{}\">{}</a></p><p>{}</p></div></div>",
            "<div class=\"site-toolbar\"><nav class=\"site-top-nav\" aria-label=\"Primary\">{}</nav>{}{}",
            "</div></header>"
        ),
        logo,
        escape_html(&site_root_href(&context.deploy_path)),
        escape_html(&context.site_title),
        escape_html(description),
        nav,
        search_button,
        theme_toggle,
    )
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
    format!("<nav class=\"site-breadcrumbs\" aria-label=\"Breadcrumbs\">{parts}</nav>")
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
    deploy_path: &str,
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
    format!(
        "<section><h2>{}</h2><ul>{items}</ul></section>",
        escape_html(title)
    )
}

fn render_local_graph_card(note: &RenderedNote) -> String {
    format!(
        concat!(
            "<section class=\"site-graph-card\" data-site-local-graph data-site-note-path=\"{}\">",
            "<h2>Local graph</h2>",
            "<p class=\"site-meta\">Published neighbors for this note, powered by the same graph asset used elsewhere in Vulcan.</p>",
            "<ul class=\"site-local-graph-list\" data-site-local-graph-list><li>Loading local graph…</li></ul>",
            "</section>"
        ),
        escape_html(&note.source_path)
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

fn build_tag_href_map(
    notes: &[ExportedNoteDocument],
    deploy_path: &str,
) -> HashMap<String, String> {
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

fn copy_asset(paths: &VaultPaths, relative: &str, destination: &Path) -> Result<bool, AppError> {
    copy_file_from_vault(paths, Path::new(relative), destination)
}

fn copy_file_from_vault(
    paths: &VaultPaths,
    relative: &Path,
    destination: &Path,
) -> Result<bool, AppError> {
    let source = paths.vault_root().join(relative);
    let contents = fs::read(source).map_err(AppError::operation)?;
    write_output_bytes_if_changed(destination, &contents)
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
      "required": ["name", "title", "deploy_path", "language", "theme", "search", "graph", "backlinks", "rss"],
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
        "rss": { "type": "boolean" }
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
      "required": ["route_manifest", "hover_previews", "recent_notes", "related_notes", "note_index", "invalidation", "schema", "typescript", "copied_assets"],
      "additionalProperties": false,
      "properties": {
        "route_manifest": { "type": "string" },
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

    fn read_site_text(root: &Path, relative: &str) -> String {
        fs::read_to_string(root.join(relative)).expect("site output should be readable")
    }

    fn read_site_json(root: &Path, relative: &str) -> Value {
        serde_json::from_str(&read_site_text(root, relative)).expect("site json should parse")
    }

    fn compact_html(value: &str) -> String {
        value.lines().map(str::trim).collect::<String>()
    }

    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        haystack.match_indices(needle).count()
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
            vault_root.join(".vulcan/site/themes/reference/nav.html"),
            "<a class=\"custom-nav\" href=\"{{home_href}}\">Portal</a>",
        )
        .expect("theme nav should write");
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
        assert!(home_html.contains("custom-nav"));
        assert!(home_html.contains("Custom footer public"));
        assert!(home_html.contains("assets/.vulcan/site/themes/reference/theme.css"));
        assert!(home_html.contains("assets/.vulcan/site/themes/reference/theme.js"));
        assert!(note_html.contains("data-site-local-graph"));
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
        assert!(
            home_html.contains(r#"data-live-reload-url="/garden/__vulcan_site/live-reload.json""#)
        );
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
            r"---
site:
  profiles:
    public:
      description: Guide summary.
---

# Guide

Body text.
",
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
        assert_eq!(
            compact_html(&note_html),
            concat!(
                "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\" />",
                "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />",
                "<title>Guide | Public Notes</title><meta name=\"description\" content=\"Guide summary.\" />",
                "<meta name=\"twitter:card\" content=\"summary\" />",
                "<meta property=\"og:title\" content=\"Guide\" />",
                "<meta property=\"og:description\" content=\"Guide summary.\" />",
                "<link rel=\"stylesheet\" href=\"/assets/vulcan-site.css\" />",
                "<script defer src=\"/assets/vulcan-site.js\"></script></head>",
                "<body data-search-asset=\"\" data-graph-asset=\"\" data-live-reload-url=\"/__vulcan_site/live-reload.json\" data-live-reload-sse-url=\"/__vulcan_site/live-reload.events\" data-current-note-path=\"Guide.md\"><a class=\"site-skip-link\" href=\"#main-content\">Skip to content</a><div class=\"site-shell\">",
                "<header class=\"site-header\"><div class=\"site-brand\"><div><p class=\"site-brand-title\"><a href=\"/\">Public Notes</a></p><p>Guide summary.</p></div></div>",
                "<div class=\"site-toolbar\"><nav class=\"site-top-nav\" aria-label=\"Primary\"><a href=\"/\">Home</a><a href=\"/recent/\">Recent</a><a href=\"/folders/\">Folders</a><a href=\"/tags/\">Tags</a></nav>",
                "<button data-theme-toggle type=\"button\" aria-label=\"Toggle color theme\" aria-pressed=\"false\">Theme</button></div></header>",
                "<div class=\"site-layout\"><main id=\"main-content\" class=\"site-content\"><article class=\"site-main\"><div class=\"site-meta\">Updated from Guide.md</div><h1 id=\"guide\">Guide</h1><p>Body text.</p></article></main>",
                "<aside class=\"site-sidebar\" aria-label=\"Page context\"><section><h2>Contents</h2><ul><li><a href=\"#guide\">Guide</a></li></ul></section></aside></div>",
                "<footer class=\"site-footer\">Built by Vulcan static site builder.</footer></div></body></html>"
            )
        );
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
        }
        assert_eq!(count_occurrences(&home_html, "<h1"), 1);
        assert_eq!(count_occurrences(&search_html, "<h1"), 1);
        assert_eq!(count_occurrences(&guide_html, "<h1"), 1);
        assert!(guide_html.contains(r#"aria-label="Breadcrumbs""#));
        assert!(search_html.contains(
            r#"<label class="site-visually-hidden" for="site-search-dialog-input">Search published notes</label>"#
        ));
        assert!(search_html.contains(r"data-site-search-open"));
        assert!(search_html.contains(r#"type="search""#));
        assert!(search_html.contains(r#"aria-describedby="site-search-dialog-hint""#));
        assert!(search_html.contains(r#"aria-keyshortcuts="/""#));
        assert!(search_html.contains(r#"aria-live="polite""#));
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
        assert!(!showcase_html.contains("Prep outline"));

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
}
