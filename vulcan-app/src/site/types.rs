use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vulcan_core::html::{HtmlRenderDiagnostic, HtmlRenderHeading};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderContext {
    pub profile: String,
    pub site_title: String,
    pub language: String,
    pub theme: String,
    pub base_url: Option<String>,
    pub deploy_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteRoute {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub title: String,
    pub slug: String,
    pub url_path: String,
    pub output_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedEmbed {
    pub kind: String,
    pub source_path: String,
    pub target_path: String,
    pub url_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_modules: Vec<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteBuildPhase {
    Planning,
    RenderingNotes,
    CopyingAssets,
    WritingSearchIndex,
    WritingGraph,
    WritingPages,
    Finalizing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteBuildProgress {
    pub phase: SiteBuildPhase,
    pub processed: usize,
    pub total: usize,
    pub current_path: Option<String>,
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
    pub shell: FrontendBundleShell,
    pub navigation: FrontendBundleNavigation,
    pub modules: FrontendBundleModules,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontendBundleArtifactPaths {
    pub route_manifest: String,
    pub navigation_tree: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontendBundleShell {
    pub reader_mode: bool,
    pub default_palette: String,
    pub left_rail: bool,
    pub right_rail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontendBundleNavigation {
    pub explorer: bool,
    pub folder_click: String,
    pub default_folder_state: String,
    pub use_saved_state: bool,
    pub show_home: bool,
    pub show_recent: bool,
    pub show_folders: bool,
    pub show_tags: bool,
    pub show_graph: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrontendBundleModules {
    pub toc: bool,
    pub graph: bool,
    pub backlinks: bool,
    pub outgoing_links: bool,
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
