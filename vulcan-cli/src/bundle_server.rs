//! `vulcan export profile serve`: a foreground host for the shared preview
//! (`vulcan_daemon::preview`) of a frontend bundle, with this module
//! supplying the build, path resolution, and live-reload payload.

use crate::CliError;
use serde_json::json;
use std::fs;
#[cfg(test)]
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use vulcan_app::site::{
    build_frontend_bundle as app_build_frontend_bundle, FrontendBundleBuildReport,
    FrontendBundleRequest,
};
use vulcan_core::VaultPaths;
use vulcan_daemon::preview::{
    percent_decode, start_preview, PreviewBuilder, PreviewOptions, PreviewSession,
};

#[derive(Debug, Clone)]
pub struct FrontendBundleServeOptions {
    pub export_profile_name: String,
    pub site_profile_name: String,
    pub output_dir: PathBuf,
    pub port: u16,
    pub debounce_ms: u64,
    pub pretty: bool,
}

pub struct FrontendBundleServeHandle {
    session: PreviewSession,
}

#[cfg(test)]
impl FrontendBundleServeHandle {
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.session.addr()
    }

    pub fn shutdown(self) -> Result<(), CliError> {
        self.session.shutdown().map_err(CliError::operation)
    }
}

pub fn serve_frontend_bundle_profile(
    paths: &VaultPaths,
    options: &FrontendBundleServeOptions,
) -> Result<(), CliError> {
    spawn_frontend_bundle_server(paths.clone(), options.clone())?
        .session
        .join()
        .map_err(CliError::operation)
}

struct BundlePreview {
    paths: VaultPaths,
    request: FrontendBundleRequest,
    export_profile_name: String,
    site_profile_name: String,
}

impl PreviewBuilder for BundlePreview {
    type Report = FrontendBundleBuildReport;

    fn label(&self) -> &'static str {
        "bundle serve"
    }

    fn kind(&self) -> &'static str {
        "bundle"
    }

    fn namespace(&self) -> &'static str {
        "__vulcan_bundle"
    }

    fn build(&self) -> Result<FrontendBundleBuildReport, String> {
        app_build_frontend_bundle(&self.paths, &self.request).map_err(|error| error.to_string())
    }

    fn output_dir(&self, report: &FrontendBundleBuildReport) -> PathBuf {
        PathBuf::from(&report.output_dir)
    }

    fn resolve(
        &self,
        output_dir: &Path,
        request_path: &str,
        _deploy_path: &str,
    ) -> Option<PathBuf> {
        resolve_bundle_path(output_dir, request_path)
    }

    fn content_type(&self, path: &Path) -> &'static str {
        content_type_for_path(path)
    }

    fn changed(
        &self,
        previous: &FrontendBundleBuildReport,
        next: &FrontendBundleBuildReport,
    ) -> bool {
        !next.changed_files.is_empty()
            || !next.deleted_files.is_empty()
            || previous.diagnostics != next.diagnostics
    }

    fn live_reload_payload(
        &self,
        report: &FrontendBundleBuildReport,
        version: u64,
        last_error: Option<&str>,
    ) -> serde_json::Value {
        json!({
            "ok": true,
            "version": version,
            "export_profile": self.export_profile_name,
            "site_profile": self.site_profile_name,
            "output_dir": report.output_dir,
            "note_count": report.note_count,
            "asset_count": report.asset_count,
            "changed_files": report.changed_files,
            "deleted_files": report.deleted_files,
            "changed_routes": report.invalidation.changed_routes,
            "deleted_routes": report.invalidation.deleted_routes,
            "changed_assets": report.invalidation.changed_assets,
            "deleted_assets": report.invalidation.deleted_assets,
            "diagnostics": report.diagnostics,
            "last_error": last_error,
        })
    }
}

#[allow(clippy::needless_pass_by_value)] // Owned options, as callers hand them over.
pub fn spawn_frontend_bundle_server(
    paths: VaultPaths,
    options: FrontendBundleServeOptions,
) -> Result<FrontendBundleServeHandle, CliError> {
    let builder = BundlePreview {
        paths: paths.clone(),
        request: FrontendBundleRequest {
            profile: Some(options.site_profile_name.clone()),
            output_dir: options.output_dir.clone(),
            clean: false,
            dry_run: false,
            pretty: options.pretty,
        },
        export_profile_name: options.export_profile_name.clone(),
        site_profile_name: options.site_profile_name.clone(),
    };
    let session = start_preview(
        &paths,
        builder,
        PreviewOptions {
            port: options.port,
            watch: true,
            debounce_ms: options.debounce_ms,
        },
    )
    .map_err(CliError::operation)?;
    Ok(FrontendBundleServeHandle { session })
}

fn resolve_bundle_path(output_dir: &Path, request_path: &str) -> Option<PathBuf> {
    let normalized = if request_path == "/" || request_path.is_empty() {
        PathBuf::from("frontend-bundle.json")
    } else {
        let trimmed = request_path.trim_start_matches('/');
        let decoded = percent_decode(trimmed);
        let mut relative = PathBuf::new();
        for component in Path::new(&decoded).components() {
            match component {
                Component::Normal(segment) => relative.push(segment),
                Component::CurDir => {}
                _ => return None,
            }
        }
        relative
    };
    let candidate = output_dir.join(&normalized);
    fs::symlink_metadata(candidate)
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
        .then_some(normalized)
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => "application/json",
        "ts" | "md" | "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    use vulcan_core::{scan_vault, ScanMode};

    #[cfg(unix)]
    #[test]
    fn bundle_preview_path_resolution_rejects_symlinked_output_files() {
        use std::os::unix::fs::symlink;

        let temp_dir = TempDir::new().expect("temp dir should create");
        let output_dir = temp_dir.path().join("output");
        let secret = temp_dir.path().join("secret.json");
        fs::create_dir(&output_dir).expect("output directory should create");
        fs::write(&secret, "{\"secret\":true}").expect("secret should write");
        symlink(&secret, output_dir.join("leak.json")).expect("symlink should create");

        assert_eq!(resolve_bundle_path(&output_dir, "/leak.json"), None);
    }

    #[test]
    fn copy_vault_tree_prepares_fresh_vulcan_dir_when_source_omits_it() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let source_root = temp_dir.path().join("source");
        let destination_root = temp_dir.path().join("destination");
        fs::create_dir_all(source_root.join("Projects")).expect("source directories should exist");
        fs::write(source_root.join("Home.md"), "# Home\n").expect("home note should be written");
        fs::write(source_root.join("Projects/Alpha.md"), "# Alpha\n")
            .expect("project note should be written");

        copy_vault_tree(&source_root, &destination_root);

        assert!(destination_root.join("Home.md").exists());
        assert!(destination_root.join("Projects/Alpha.md").exists());
        assert!(destination_root.join(".vulcan").is_dir());
    }

    #[test]
    fn bundle_serve_serves_contract_and_live_reload_state() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_frontend_bundle_server(
            VaultPaths::new(&vault_root),
            FrontendBundleServeOptions {
                export_profile_name: "public_bundle".to_string(),
                site_profile_name: "public".to_string(),
                output_dir: vault_root.join("exports/public-bundle"),
                port: 0,
                debounce_ms: 50,
                pretty: true,
            },
        )
        .expect("bundle server should start");

        let contract = get_json(handle.addr(), "/frontend-bundle.json");
        let live = get_json(handle.addr(), "/__vulcan_bundle/live-reload.json");
        let note = get_json(handle.addr(), "/notes/home/index.json");

        assert_eq!(contract["contract"]["name"], "vulcan_frontend_bundle");
        assert_eq!(live["ok"], true);
        assert_eq!(live["version"], 1);
        assert!(note["body_html"]
            .as_str()
            .is_some_and(|html| html.contains("Home")));

        handle.shutdown().expect("bundle server should shut down");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "FSEvents does not reliably deliver events in CI"
    )]
    fn bundle_serve_watch_rebuilds_output_and_bumps_live_reload_version() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"[site.profiles.public]
title = "Public Notes"
home = "Home"
include_paths = ["Home.md", "Projects/Alpha.md"]
search = true
graph = true
"#,
        )
        .expect("config should be written");
        scan_vault(&VaultPaths::new(&vault_root), ScanMode::Full).expect("scan should succeed");

        let handle = spawn_frontend_bundle_server(
            VaultPaths::new(&vault_root),
            FrontendBundleServeOptions {
                export_profile_name: "public_bundle".to_string(),
                site_profile_name: "public".to_string(),
                output_dir: vault_root.join("exports/public-bundle"),
                port: 0,
                debounce_ms: 50,
                pretty: true,
            },
        )
        .expect("bundle server should start");

        thread::sleep(Duration::from_millis(300));
        let initial_live = get_json(handle.addr(), "/__vulcan_bundle/live-reload.json");
        let initial_version = initial_live["version"]
            .as_u64()
            .expect("live version should be numeric");

        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Start\n---\n\n# Home\n\nBundle preview lives here.\n",
        )
        .expect("updated note should be written");

        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        let mut updated = None;
        while std::time::Instant::now() < deadline {
            let live = get_json(handle.addr(), "/__vulcan_bundle/live-reload.json");
            if live["version"].as_u64().unwrap_or_default() > initial_version {
                let note = get_json(handle.addr(), "/notes/home/index.json");
                if note["body_html"]
                    .as_str()
                    .is_some_and(|html| html.contains("Bundle preview lives here."))
                {
                    updated = Some(live);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }

        assert!(
            updated.is_some(),
            "bundle output should refresh under watch"
        );
        handle.shutdown().expect("bundle server should shut down");
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_vault_tree(&source, destination);
    }

    fn copy_vault_tree(source: &Path, destination: &Path) {
        copy_dir_recursive(source, destination);
        fs::create_dir_all(destination.join(".vulcan")).expect(".vulcan dir should be created");
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");
        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            if entry.file_name() == ".vulcan" {
                continue;
            }
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            if entry
                .file_type()
                .expect("file type should be readable")
                .is_dir()
            {
                copy_dir_recursive(&source_path, &destination_path);
            } else {
                if let Some(parent) = destination_path.parent() {
                    fs::create_dir_all(parent).expect("parent directory should be created");
                }
                fs::copy(&source_path, &destination_path)
                    .expect("fixture file should copy successfully");
            }
        }
    }

    fn get_json(addr: SocketAddr, path: &str) -> Value {
        let raw = get_text(addr, path);
        serde_json::from_str(&raw).expect("response should be valid JSON")
    }

    fn get_text(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("TCP connection should succeed");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .expect("request should write");
        stream.flush().expect("request should flush");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("response should read");
        let text = String::from_utf8(response).expect("response should be valid UTF-8");
        text.split("\r\n\r\n")
            .nth(1)
            .unwrap_or_default()
            .to_string()
    }
}
