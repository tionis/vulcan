//! `vulcan site serve`: a foreground host for the shared static-site
//! preview (`vulcan_daemon::preview`), with this module supplying the build,
//! path resolution, and live-reload payload.

use crate::CliError;
use serde_json::json;
use std::fs;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use vulcan_app::site::{
    build_site_with_filter as app_build_site_with_filter,
    build_site_with_filter_and_progress as app_build_site_with_filter_and_progress,
    SiteBuildProgress, SiteBuildReport, SiteBuildRequest,
};
use vulcan_core::permissions::PermissionFilter;
use vulcan_core::VaultPaths;
use vulcan_daemon::preview::{
    percent_decode, start_preview, PreviewBuilder, PreviewOptions, PreviewSession,
};

#[derive(Debug, Clone)]
pub struct SiteServeOptions {
    pub profile: Option<String>,
    pub output_dir: Option<PathBuf>,
    pub port: u16,
    pub watch: bool,
    pub debounce_ms: u64,
    pub strict: bool,
    pub fail_on_warning: bool,
    pub read_filter: Option<PermissionFilter>,
}

pub struct SiteServeHandle {
    session: PreviewSession,
}

impl SiteServeHandle {
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.session.addr()
    }

    pub fn join(self) -> Result<(), CliError> {
        self.session.join().map_err(CliError::operation)
    }
}

#[cfg(test)]
impl SiteServeHandle {
    pub fn shutdown(self) -> Result<(), CliError> {
        self.session.shutdown().map_err(CliError::operation)
    }
}

pub(crate) fn site_build_policy_error(
    report: &SiteBuildReport,
    strict: bool,
    fail_on_warning: bool,
) -> Option<String> {
    if !(strict || fail_on_warning) {
        return None;
    }
    let diagnostics = report
        .diagnostics
        .iter()
        .filter(|diagnostic| matches!(diagnostic.level.as_str(), "warn" | "error"))
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        return None;
    }
    let preview = diagnostics
        .iter()
        .take(3)
        .map(|diagnostic| match diagnostic.source_path.as_deref() {
            Some(path) => format!(
                "[{}] {} {} ({path})",
                diagnostic.level, diagnostic.kind, diagnostic.message
            ),
            None => format!(
                "[{}] {} {}",
                diagnostic.level, diagnostic.kind, diagnostic.message
            ),
        })
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "site build for profile `{}` reported {} publish diagnostic(s): {}",
        report.profile,
        diagnostics.len(),
        preview
    ))
}

pub(crate) fn build_site_with_policy(
    paths: &VaultPaths,
    request: &SiteBuildRequest,
    strict: bool,
    fail_on_warning: bool,
    read_filter: Option<&PermissionFilter>,
) -> Result<SiteBuildReport, CliError> {
    build_site_with_policy_and_progress(
        paths,
        request,
        strict,
        fail_on_warning,
        read_filter,
        |_| {},
    )
}

pub(crate) fn build_site_with_policy_and_progress<F>(
    paths: &VaultPaths,
    request: &SiteBuildRequest,
    strict: bool,
    fail_on_warning: bool,
    read_filter: Option<&PermissionFilter>,
    mut progress: F,
) -> Result<SiteBuildReport, CliError>
where
    F: FnMut(&SiteBuildProgress),
{
    if strict || fail_on_warning {
        let mut preflight = request.clone();
        preflight.dry_run = true;
        let preflight_report = app_build_site_with_filter(paths, &preflight, read_filter)
            .map_err(CliError::operation)?;
        if let Some(message) = site_build_policy_error(&preflight_report, strict, fail_on_warning) {
            return Err(CliError::operation(message));
        }
    }
    app_build_site_with_filter_and_progress(paths, request, read_filter, |event| progress(event))
        .map_err(CliError::operation)
}

struct SitePreview {
    paths: VaultPaths,
    request: SiteBuildRequest,
    strict: bool,
    fail_on_warning: bool,
    read_filter: Option<PermissionFilter>,
}

impl PreviewBuilder for SitePreview {
    type Report = SiteBuildReport;

    fn label(&self) -> &'static str {
        "site serve"
    }

    fn kind(&self) -> &'static str {
        "site"
    }

    fn namespace(&self) -> &'static str {
        "__vulcan_site"
    }

    fn build(&self) -> Result<SiteBuildReport, String> {
        build_site_with_policy(
            &self.paths,
            &self.request,
            self.strict,
            self.fail_on_warning,
            self.read_filter.as_ref(),
        )
        .map_err(|error| error.to_string())
    }

    fn output_dir(&self, report: &SiteBuildReport) -> PathBuf {
        PathBuf::from(&report.output_dir)
    }

    fn deploy_path(&self, report: &SiteBuildReport) -> String {
        report.deploy_path.clone()
    }

    fn resolve(&self, output_dir: &Path, request_path: &str, deploy_path: &str) -> Option<PathBuf> {
        resolve_site_path(output_dir, request_path, deploy_path)
    }

    fn content_type(&self, path: &Path) -> &'static str {
        content_type_for_path(path)
    }

    fn changed(&self, previous: &SiteBuildReport, next: &SiteBuildReport) -> bool {
        !next.changed_files.is_empty()
            || !next.deleted_files.is_empty()
            || previous.diagnostics != next.diagnostics
    }

    fn live_reload_payload(
        &self,
        report: &SiteBuildReport,
        version: u64,
        last_error: Option<&str>,
    ) -> serde_json::Value {
        let diagnostics = report
            .diagnostics
            .iter()
            .filter(|diagnostic| matches!(diagnostic.level.as_str(), "warn" | "error"))
            .collect::<Vec<_>>();
        json!({
            "ok": true,
            "version": version,
            "profile": report.profile,
            "note_count": report.note_count,
            "page_count": report.page_count,
            "asset_count": report.asset_count,
            "changed_files": report.changed_files,
            "deleted_files": report.deleted_files,
            "diagnostics": diagnostics,
            "last_error": last_error,
        })
    }

    fn rebuilt(&self, report: &SiteBuildReport) {
        log_watch_rebuild_success(report);
    }

    fn rebuild_failed(&self, error: &str) {
        eprintln!("site serve rebuild failed: {error}");
    }
}

#[allow(clippy::needless_pass_by_value)] // Owned options, as callers hand them over.
pub fn spawn_site_server(
    paths: VaultPaths,
    options: SiteServeOptions,
) -> Result<SiteServeHandle, CliError> {
    let builder = SitePreview {
        paths: paths.clone(),
        request: SiteBuildRequest {
            profile: options.profile.clone(),
            output_dir: options.output_dir.clone(),
            clean: false,
            dry_run: false,
        },
        strict: options.strict,
        fail_on_warning: options.fail_on_warning,
        read_filter: options.read_filter.clone(),
    };
    let session = start_preview(
        &paths,
        builder,
        PreviewOptions {
            port: options.port,
            watch: options.watch,
            debounce_ms: options.debounce_ms,
        },
    )
    .map_err(CliError::operation)?;
    Ok(SiteServeHandle { session })
}

fn log_watch_rebuild_success(report: &SiteBuildReport) {
    let diagnostics = report
        .diagnostics
        .iter()
        .filter(|diagnostic| matches!(diagnostic.level.as_str(), "warn" | "error"))
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        if !report.changed_files.is_empty() || !report.deleted_files.is_empty() {
            eprintln!(
                "site serve rebuilt `{}`: {} changed, {} deleted",
                report.profile,
                report.changed_files.len(),
                report.deleted_files.len()
            );
        }
        return;
    }
    let preview = diagnostics
        .iter()
        .take(3)
        .map(|diagnostic| match diagnostic.source_path.as_deref() {
            Some(path) => format!(
                "[{}] {} {} ({path})",
                diagnostic.level, diagnostic.kind, diagnostic.message
            ),
            None => format!(
                "[{}] {} {}",
                diagnostic.level, diagnostic.kind, diagnostic.message
            ),
        })
        .collect::<Vec<_>>()
        .join("; ");
    eprintln!(
        "site serve rebuilt `{}` with {} publish diagnostic(s): {}",
        report.profile,
        diagnostics.len(),
        preview
    );
}

fn resolve_site_path(output_dir: &Path, request_path: &str, deploy_path: &str) -> Option<PathBuf> {
    let relative_request = strip_deploy_path(request_path, deploy_path).unwrap_or(request_path);
    let normalized = if relative_request.is_empty() || relative_request == "/" {
        PathBuf::from("index.html")
    } else {
        let trimmed = relative_request.trim_start_matches('/');
        let decoded = percent_decode(trimmed);
        let mut relative = PathBuf::new();
        for component in Path::new(&decoded).components() {
            match component {
                Component::Normal(segment) => relative.push(segment),
                Component::CurDir => {}
                _ => return None,
            }
        }
        if relative_request.ends_with('/') {
            relative.push("index.html");
        }
        relative
    };

    let direct = output_dir.join(&normalized);
    if is_regular_file_no_symlink(&direct) {
        return Some(normalized);
    }

    if direct.is_dir() {
        let nested = normalized.join("index.html");
        if is_regular_file_no_symlink(&output_dir.join(&nested)) {
            return Some(nested);
        }
    }

    if normalized.extension().is_none() {
        let nested = normalized.join("index.html");
        if is_regular_file_no_symlink(&output_dir.join(&nested)) {
            return Some(nested);
        }
    }

    None
}

fn is_regular_file_no_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
}

fn strip_deploy_path<'a>(request_path: &'a str, deploy_path: &str) -> Option<&'a str> {
    if deploy_path.is_empty() {
        return Some(request_path);
    }
    if request_path == deploy_path {
        return Some("/");
    }
    request_path.strip_prefix(deploy_path)
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    use serde_json::Value;
    use std::fs;
    #[allow(unused_imports)]
    use std::io::{Read, Write};
    #[allow(unused_imports)]
    use std::net::TcpStream;
    #[allow(unused_imports)]
    use std::thread;
    #[allow(unused_imports)]
    use std::time::Duration;
    use tempfile::TempDir;

    // Native filesystem events are hints. Leave time for the watcher's 30-second
    // safety reconciliation and the rebuild on hosts that silently lose events.
    const WATCH_REBUILD_TIMEOUT: Duration = Duration::from_secs(45);
    use vulcan_core::{scan_vault, ScanMode};

    #[cfg(unix)]
    #[test]
    fn preview_path_resolution_rejects_symlinked_output_files() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().expect("temp dir should create");
        let output_dir = temp_dir.path().join("output");
        let secret = temp_dir.path().join("secret.txt");
        fs::create_dir(&output_dir).expect("output directory should create");
        fs::write(&secret, "host secret").expect("secret should write");
        symlink(&secret, output_dir.join("leak.txt")).expect("symlink should create");

        assert_eq!(resolve_site_path(&output_dir, "/leak.txt", ""), None);
    }

    #[test]
    fn site_serve_serves_static_output_and_live_reload_state() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
base_url = "https://notes.example.com"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
rss = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: false,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
                read_filter: None,
            },
        )
        .expect("site server should start");

        let index = get_text(handle.addr(), "/");
        let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
        let search = get_json(handle.addr(), "/assets/search-index.json");

        assert!(index.contains("Public Notes"));
        assert!(index.contains("Built by Vulcan static site builder"));
        assert_eq!(live["ok"], true);
        assert_eq!(live["version"], 1);
        assert!(live["last_error"].is_null());
        assert_eq!(search["version"], 2);
        assert!(search["documents"]
            .as_array()
            .is_some_and(|documents| !documents.is_empty()));

        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "FSEvents does not reliably deliver events in CI"
    )]
    fn site_serve_watch_rebuilds_output_and_bumps_live_reload_version() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
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
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: true,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
                read_filter: None,
            },
        )
        .expect("site server should start");

        let initial_live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
        let initial_version = initial_live["version"]
            .as_u64()
            .expect("live version should be numeric");
        let before = get_text(handle.addr(), "/notes/home/");
        assert!(!before.contains("Moonshot preview"));

        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Start\n---\n\n# Home\n\nMoonshot preview lives here.\n",
        )
        .expect("updated note should be written");

        let mut reloaded_html = None;
        let deadline = std::time::Instant::now() + WATCH_REBUILD_TIMEOUT;
        while std::time::Instant::now() < deadline {
            let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
            if live["version"].as_u64().unwrap_or_default() > initial_version {
                let html = get_text(handle.addr(), "/notes/home/");
                if html.contains("Moonshot preview lives here.") {
                    reloaded_html = Some(html);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        assert!(
            reloaded_html.is_some(),
            "watch-backed site output should refresh"
        );
        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "FSEvents does not reliably deliver events in CI"
    )]
    fn site_serve_watch_strict_keeps_last_good_output_on_publish_diagnostic() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Home.md"),
            "# Home

Baseline public page.
",
        )
        .expect("home note should write");
        fs::write(
            vault_root.join("Private.md"),
            "---
tags:
  - private
---

# Private

Hidden note.
",
        )
        .expect("private note should write");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Private.md"]
exclude_tags = ["private"]
link_policy = "warn"
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: true,
                debounce_ms: 50,
                strict: true,
                fail_on_warning: false,
                read_filter: None,
            },
        )
        .expect("site server should start");

        let initial_live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
        let initial_version = initial_live["version"]
            .as_u64()
            .expect("live version should be numeric");
        let before = get_text(handle.addr(), "/notes/home/");
        assert!(before.contains("Baseline public page."));

        fs::write(
            vault_root.join("Home.md"),
            "# Home

Strict preview should block this. See [[Private]].
",
        )
        .expect("updated note should be written");

        let deadline = std::time::Instant::now() + WATCH_REBUILD_TIMEOUT;
        let mut observed_error = None;
        while std::time::Instant::now() < deadline {
            let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
            if live["last_error"].as_str().is_some() {
                observed_error = Some(live);
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        let live = observed_error.expect("strict watch mode should report a publish diagnostic");
        assert!(
            live["version"].as_u64().unwrap_or_default() > initial_version,
            "live reload version should advance when strict-mode diagnostics change: initial={}, live={}",
            initial_version,
            live["version"].as_u64().unwrap_or_default()
        );
        assert!(live["last_error"]
            .as_str()
            .is_some_and(|message| message.contains("publish diagnostic")));
        let after = get_text(handle.addr(), "/notes/home/");
        assert!(after.contains("Baseline public page."));
        assert!(!after.contains("Strict preview should block this."));

        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    fn site_serve_live_reload_payload_includes_publish_diagnostics() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Home.md"),
            "# Home\n\nThis page links to [[Private]].\n",
        )
        .expect("home note should write");
        fs::write(
            vault_root.join("Private.md"),
            "---\ntags:\n  - private\n---\n\n# Private\n",
        )
        .expect("private note should write");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Private.md"]
exclude_tags = ["private"]
link_policy = "warn"
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: false,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
                read_filter: None,
            },
        )
        .expect("site server should start");

        let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
        let diagnostics = live["diagnostics"]
            .as_array()
            .expect("diagnostics should be an array");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["kind"], "unpublished_link_target");
        assert!(live["changed_files"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty()));

        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "FSEvents does not reliably deliver events in CI"
    )]
    fn site_serve_watch_streams_sse_live_reload_updates() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
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
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: true,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
                read_filter: None,
            },
        )
        .expect("site server should start");

        let mut initial_stream =
            open_sse_stream(handle.addr(), "/__vulcan_site/live-reload.events");
        let initial = read_sse_event(&mut initial_stream);
        let initial_version = initial["version"]
            .as_u64()
            .expect("initial SSE version should be numeric");
        drop(initial_stream);

        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Start\n---\n\n# Home\n\nSSE preview lives here.\n",
        )
        .expect("updated note should be written");

        let deadline = std::time::Instant::now() + WATCH_REBUILD_TIMEOUT;
        let mut updated_version = None;
        while std::time::Instant::now() < deadline {
            let live = get_json(handle.addr(), "/__vulcan_site/live-reload.json");
            if live["version"].as_u64().unwrap_or_default() > initial_version {
                updated_version = live["version"].as_u64();
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        let updated_version = updated_version.expect("watch build should advance the live version");
        let mut updated_stream =
            open_sse_stream(handle.addr(), "/__vulcan_site/live-reload.events");
        let event = read_sse_event(&mut updated_stream);
        assert_eq!(
            event["version"].as_u64().unwrap_or_default(),
            updated_version
        );
        assert!(event["changed_files"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|value| {
                value
                    .as_str()
                    .is_some_and(|path| path.contains("notes/home/index.html"))
            })));

        handle.shutdown().expect("site server should shut down");
    }

    #[test]
    fn site_serve_supports_prefixed_routes_and_live_reload_endpoints() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
base_url = "https://notes.example.com"
deploy_path = "/garden"
home = "Home"
output_dir = ".vulcan/site/public"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_site_server(
            VaultPaths::new(&vault_root),
            SiteServeOptions {
                profile: Some("public".to_string()),
                output_dir: None,
                port: 0,
                watch: false,
                debounce_ms: 50,
                strict: false,
                fail_on_warning: false,
                read_filter: None,
            },
        )
        .expect("site server should start");

        let root_index = get_text(handle.addr(), "/");
        let prefixed_index = get_text(handle.addr(), "/garden/");
        let prefixed_note = get_text(handle.addr(), "/garden/notes/home/");
        let prefixed_live = get_json(handle.addr(), "/garden/__vulcan_site/live-reload.json");

        assert!(root_index.contains(r#"href="/garden/""#));
        assert!(prefixed_index.contains(r#"href="/garden/assets/vulcan-site.css""#));
        assert!(prefixed_note.contains("Home links to"));
        assert_eq!(prefixed_live["ok"], true);
        assert_eq!(prefixed_live["profile"], "public");

        handle.shutdown().expect("site server should shut down");
    }

    fn copy_fixture_vault(name: &str, destination: &std::path::Path) {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_recursive(&source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
    }

    fn copy_dir_recursive(source: &std::path::Path, destination: &std::path::Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("parent directory should exist");
                }
                fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }

    fn get_text(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("server should accept connections");
        let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .expect("request should write");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("response should read");
        response
            .split("\r\n\r\n")
            .nth(1)
            .expect("response should contain body")
            .to_string()
    }

    fn get_json(addr: SocketAddr, path: &str) -> Value {
        let body = get_text(addr, path);
        serde_json::from_str(&body).expect("response body should parse as JSON")
    }

    fn open_sse_stream(addr: SocketAddr, path: &str) -> TcpStream {
        let mut stream = TcpStream::connect(addr).expect("server should accept connections");
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .expect("SSE request should write");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("SSE read timeout should set");
        stream
    }

    fn read_sse_event(stream: &mut TcpStream) -> Value {
        let mut buffer = Vec::new();
        let mut headers_done = false;
        loop {
            let mut chunk = [0_u8; 1024];
            let bytes = stream
                .read(&mut chunk)
                .expect("SSE stream should be readable");
            assert!(bytes > 0, "SSE stream closed before an event arrived");
            buffer.extend_from_slice(&chunk[..bytes]);
            if !headers_done {
                if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
                    buffer.drain(..index + 4);
                    headers_done = true;
                } else {
                    continue;
                }
            }
            if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
                let frame = String::from_utf8_lossy(&buffer[..index]).to_string();
                buffer.drain(..index + 4);
                if frame.starts_with(':') {
                    continue;
                }
                if let Some(payload) = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("data: ").map(ToOwned::to_owned))
                {
                    return serde_json::from_str(&payload).expect("SSE event payload should parse");
                }
            }
        }
    }
}
