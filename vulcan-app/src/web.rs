use crate::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use vulcan_core::{
    fetch_web_content, load_vault_config, prepare_search_backend, search_web,
    PreparedWebSearchBackend, SearchBackendKind, VaultPaths,
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

pub fn apply_web_fetch_report(
    paths: &VaultPaths,
    request: &WebFetchRequest,
) -> Result<WebFetchReport, AppError> {
    let config = load_vault_config(paths).config.web;
    let mut fetched = fetch_web_content(&config, &request.url, request.mode.as_str())
        .map_err(AppError::operation)?;

    if let Some(path) = request.save.as_ref() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(AppError::operation)?;
        }
        match request.mode {
            WebFetchMode::Raw => {
                fs::write(path, &fetched.raw_bytes).map_err(AppError::operation)?;
            }
            WebFetchMode::Html | WebFetchMode::Markdown => {
                fs::write(path, fetched.report.content.as_bytes()).map_err(AppError::operation)?;
            }
        }
        fetched.report.saved = Some(path.display().to_string());
    }

    Ok(fetched.report)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_web_fetch_report, build_web_search_report, prepare_web_search, WebFetchMode,
        WebFetchRequest, WebSearchRequest,
    };
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, VaultPaths};

    fn test_paths() -> (tempfile::TempDir, VaultPaths) {
        let dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        (dir, paths)
    }

    #[test]
    fn prepare_web_search_uses_configured_endpoint() {
        let (_dir, paths) = test_paths();
        fs::write(
            paths.config_file(),
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
            paths.config_file(),
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
                save: Some(destination.clone()),
            },
        )
        .expect("fetch should succeed");
        handle.join().expect("server thread should finish");

        assert_eq!(report.status, 200);
        assert_eq!(report.mode, "raw");
        let saved = destination.display().to_string();
        assert_eq!(report.saved.as_deref(), Some(saved.as_str()));
        assert_eq!(fs::read(&destination).expect("saved bytes"), b"raw-body");
    }
}
