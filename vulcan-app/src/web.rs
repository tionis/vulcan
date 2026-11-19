use crate::AppError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vulcan_core::paths::{normalize_relative_input_path, secure_write, RelativePathOptions};
use vulcan_core::{
    fetch_web_content, load_vault_config, prepare_search_backend, search_web, PermissionGuard,
    PreparedWebSearchBackend, ProfilePermissionGuard, SearchBackendKind, VaultPaths,
};

pub use vulcan_core::{WebFetchReport, WebSearchReport};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchRequest {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<SearchBackendKind>,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWebSearchRequest {
    pub backend: String,
    pub base_url: String,
    pub query: String,
    pub limit: usize,
    prepared_backend: PreparedWebSearchBackend,
    user_agent: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebFetchMode {
    #[default]
    Markdown,
    Html,
    Raw,
}

impl WebFetchMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Html => "html",
            Self::Raw => "raw",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFetchRequest {
    pub url: String,
    #[serde(default)]
    pub mode: WebFetchMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save: Option<PathBuf>,
}

const fn default_search_limit() -> usize {
    10
}

pub fn prepare_web_search(
    paths: &VaultPaths,
    request: &WebSearchRequest,
) -> Result<PreparedWebSearchRequest, AppError> {
    let config = load_vault_config(paths).config.web;
    let prepared = prepare_search_backend(&config, request.backend).map_err(AppError::operation)?;
    Ok(PreparedWebSearchRequest {
        backend: prepared.backend.clone(),
        base_url: prepared.base_url.clone(),
        query: request.query.clone(),
        limit: request.limit,
        prepared_backend: prepared,
        user_agent: config.user_agent,
    })
}

pub fn execute_web_search(
    prepared: &PreparedWebSearchRequest,
) -> Result<WebSearchReport, AppError> {
    search_web(
        &prepared.user_agent,
        &prepared.prepared_backend,
        &prepared.query,
        prepared.limit,
    )
    .map_err(AppError::operation)
}

pub fn build_web_search_report(
    paths: &VaultPaths,
    request: &WebSearchRequest,
) -> Result<WebSearchReport, AppError> {
    let prepared = prepare_web_search(paths, request)?;
    execute_web_search(&prepared)
}

/// Enforce the caller's network boundary before issuing a search request.
pub fn build_web_search_report_with_permissions(
    paths: &VaultPaths,
    request: &WebSearchRequest,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<WebSearchReport, AppError> {
    let prepared = prepare_web_search(paths, request)?;
    if let Some(permissions) = permissions {
        permissions
            .check_network(&prepared.base_url)
            .map_err(AppError::operation)?;
    }
    execute_web_search(&prepared)
}

/// Check both network and optional vault-write authority before fetching.
pub fn apply_web_fetch_report_with_permissions(
    paths: &VaultPaths,
    request: &WebFetchRequest,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<WebFetchReport, AppError> {
    if let Some(permissions) = permissions {
        permissions
            .check_network(&request.url)
            .map_err(AppError::operation)?;
    }
    let save = request
        .save
        .as_ref()
        .map(|path| {
            normalize_relative_input_path(
                &path.to_string_lossy(),
                RelativePathOptions {
                    expected_extension: None,
                    append_extension_if_missing: false,
                },
            )
            .map(PathBuf::from)
            .map_err(AppError::operation)
        })
        .transpose()?;
    if let (Some(permissions), Some(save)) = (permissions, save.as_ref()) {
        permissions
            .check_write_path(&save.to_string_lossy())
            .map_err(AppError::operation)?;
    }
    apply_web_fetch_report_as(
        paths,
        &WebFetchRequest {
            url: request.url.clone(),
            mode: request.mode,
            save,
        },
        permissions.map(|guard| guard.selection().name.as_str()),
    )
}

pub fn apply_web_fetch_report(
    paths: &VaultPaths,
    request: &WebFetchRequest,
) -> Result<WebFetchReport, AppError> {
    apply_web_fetch_report_as(paths, request, None)
}

fn apply_web_fetch_report_as(
    paths: &VaultPaths,
    request: &WebFetchRequest,
    permission_profile: Option<&str>,
) -> Result<WebFetchReport, AppError> {
    let config = load_vault_config(paths).config.web;
    let mut fetched = fetch_web_content(&config, &request.url, request.mode.as_str())
        .map_err(AppError::operation)?;

    if let Some(path) = request.save.as_ref() {
        let normalized = normalize_relative_input_path(
            &path.to_string_lossy(),
            RelativePathOptions {
                expected_extension: None,
                append_extension_if_missing: false,
            },
        )
        .map_err(AppError::operation)?;
        let relative = std::path::Path::new(&normalized);
        let bytes = match request.mode {
            WebFetchMode::Raw => fetched.raw_bytes.as_slice(),
            WebFetchMode::Html | WebFetchMode::Markdown => fetched.report.content.as_bytes(),
        };
        save_fetched(paths, &normalized, bytes, permission_profile)?;
        fetched.report.saved = Some(paths.vault_root().join(relative).display().to_string());
    }

    Ok(fetched.report)
}

/// Save fetched bytes. A note an mdbase collection manages is written
/// through a validated mdbase write; any other file directly.
fn save_fetched(
    paths: &VaultPaths,
    path: &str,
    bytes: &[u8],
    permission_profile: Option<&str>,
) -> Result<(), AppError> {
    if crate::notes::note_path_is_mdbase_managed(paths, path, permission_profile)? {
        let after = std::str::from_utf8(bytes).map_err(|_| {
            AppError::operation(format!(
                "{path} is a record of the mdbase collection; fetched content that is not UTF-8 text cannot be saved there"
            ))
        })?;
        let before = crate::notes::read_note_for_update(paths, path)?;
        let changes = [vulcan_core::ordinary_write::OrdinaryWriteChange {
            path: path.to_string(),
            before,
            after: Some(after.to_string()),
        }];
        crate::note_writes::apply_planned_note_write(
            paths,
            &crate::note_writes::PlannedNoteWrite::new(&changes, permission_profile),
        )?;
        return Ok(());
    }
    secure_write(paths.vault_root(), std::path::Path::new(path), bytes).map_err(AppError::operation)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_web_fetch_report, apply_web_fetch_report_with_permissions, build_web_search_report,
        build_web_search_report_with_permissions, prepare_web_search, WebFetchMode,
        WebFetchRequest, WebSearchRequest,
    };
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;
    use tempfile::tempdir;
    use vulcan_core::{
        initialize_vulcan_dir, resolve_permission_profile, ProfilePermissionGuard,
        SearchBackendKind, VaultPaths,
    };

    fn test_paths() -> (tempfile::TempDir, VaultPaths) {
        let dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        (dir, paths)
    }

    #[test]
    fn permission_aware_web_workflows_deny_before_network_access() {
        let (_dir, paths) = test_paths();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).expect("readonly profile"),
        );
        let search = build_web_search_report_with_permissions(
            &paths,
            &WebSearchRequest {
                query: "docs".to_string(),
                backend: Some(SearchBackendKind::Duckduckgo),
                limit: 1,
            },
            Some(&guard),
        )
        .expect_err("readonly search must be denied");
        assert!(search.to_string().contains("network"));

        let fetch = apply_web_fetch_report_with_permissions(
            &paths,
            &WebFetchRequest {
                url: "http://127.0.0.1:1/private".to_string(),
                mode: WebFetchMode::Markdown,
                save: None,
            },
            Some(&guard),
        )
        .expect_err("readonly fetch must be denied");
        assert!(fetch.to_string().contains("network"));
    }

    #[test]
    fn saving_into_a_collection_record_is_a_validated_mdbase_write() {
        let (dir, paths) = test_paths();
        let root = dir.path();
        fs::write(root.join("mdbase.yaml"), "spec_version: '0.3.0'\n").unwrap();
        fs::create_dir_all(root.join("_types")).unwrap();
        fs::write(
            root.join("_types/page.md"),
            "---\nkind: mdbase.type\nname: page\nmatch:\n  where:\n    type: page\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [url]\n---\n",
        )
        .unwrap();
        let error = super::save_fetched(&paths, "Clip.md", b"---\ntype: page\n---\nBody\n", None)
            .expect_err("a page without its url is rejected");
        assert!(error.to_string().contains("required"), "{error}");
        assert!(!root.join("Clip.md").exists());

        super::save_fetched(
            &paths,
            "Clip.md",
            b"---\ntype: page\nurl: x\n---\nBody\n",
            None,
        )
        .expect("valid page");
        assert_eq!(
            vulcan_core::mdbase::list_mdbase_write_outbox(&paths)
                .unwrap()
                .len(),
            1
        );
        assert!(super::save_fetched(&paths, "Clip.md", b"\xff\xfe", None).is_err());
        // Files outside the record extensions are written as before.
        super::save_fetched(&paths, "image.bin", b"\xff\xfe", None).expect("binary file");
        assert_eq!(fs::read(root.join("image.bin")).unwrap(), b"\xff\xfe");
    }

    #[test]
    fn invalid_web_fetch_save_path_is_rejected_before_fetching() {
        let (_dir, paths) = test_paths();
        let error = apply_web_fetch_report_with_permissions(
            &paths,
            &WebFetchRequest {
                url: "http://127.0.0.1:1/test".to_string(),
                mode: WebFetchMode::Raw,
                save: Some(PathBuf::from("../outside.txt")),
            },
            None,
        )
        .expect_err("invalid save path must fail before networking");
        assert!(!error.to_string().contains("connection"));
    }

    #[test]
    fn prepare_web_search_uses_configured_endpoint() {
        let (_dir, paths) = test_paths();
        fs::write(
            paths.local_config_file(),
            r#"[web.search]
base_url = "http://127.0.0.1:4455/search"
"#,
        )
        .expect("config should be written");

        let prepared = prepare_web_search(
            &paths,
            &WebSearchRequest {
                query: "release notes".to_string(),
                backend: None,
                limit: 5,
            },
        )
        .expect("search should prepare");

        assert_eq!(prepared.backend, "duckduckgo");
        assert_eq!(prepared.base_url, "http://127.0.0.1:4455/search");
        assert_eq!(prepared.limit, 5);
    }

    #[test]
    fn build_web_search_report_uses_shared_workflow() {
        let (_dir, paths) = test_paths();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener
            .local_addr()
            .expect("listener should expose address");
        fs::write(
            paths.local_config_file(),
            format!(
                "[web.search]\nbase_url = \"http://{address}/search\"\nbackend = \"duckduckgo\"\n"
            ),
        )
        .expect("config should be written");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection should be accepted");
            let mut buffer = [0_u8; 4096];
            let _ = stream
                .read(&mut buffer)
                .expect("request should be readable");
            let body = r#"
<html><body>
  <a class="result__a" href="https://example.com/docs">Example Docs</a>
  <div class="result__snippet">Shared web workflow result.</div>
</body></html>
"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should be writable");
        });

        let report = build_web_search_report(
            &paths,
            &WebSearchRequest {
                query: "docs".to_string(),
                backend: None,
                limit: 1,
            },
        )
        .expect("search should succeed");
        handle.join().expect("server thread should finish");

        assert_eq!(report.backend, "duckduckgo");
        assert_eq!(report.query, "docs");
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].title, "Example Docs");
        assert_eq!(report.results[0].url, "https://example.com/docs");
    }

    #[test]
    fn apply_web_fetch_report_saves_raw_content() {
        let (_dir, paths) = test_paths();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener
            .local_addr()
            .expect("listener should expose address");
        let handle = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("connection should be accepted");
                let mut buffer = [0_u8; 2048];
                let read = stream
                    .read(&mut buffer)
                    .expect("request should be readable");
                let request = String::from_utf8_lossy(&buffer[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (content_type, body) = if path == "/robots.txt" {
                    ("text/plain", b"User-agent: *\nAllow: /\n".as_slice())
                } else {
                    ("application/octet-stream", b"raw-body".as_slice())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("response header should be writable");
                stream
                    .write_all(body)
                    .expect("response body should be writable");
            }
        });

        let destination = paths.vault_root().join("downloads").join("page.bin");
        let report = apply_web_fetch_report(
            &paths,
            &WebFetchRequest {
                url: format!("http://{address}/raw"),
                mode: WebFetchMode::Raw,
                save: Some(std::path::PathBuf::from("downloads/page.bin")),
            },
        )
        .expect("fetch should succeed");
        handle.join().expect("server thread should finish");

        assert_eq!(report.status, 200);
        assert_eq!(report.mode, "raw");
        assert_eq!(
            report.saved.as_deref().map(std::path::Path::new),
            Some(destination.as_path())
        );
        assert_eq!(fs::read(&destination).expect("saved bytes"), b"raw-body");
    }
}
