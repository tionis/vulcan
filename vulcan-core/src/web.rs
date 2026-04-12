use crate::config::{SearchBackendKind, WebConfig};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::AUTHORIZATION;
use rs_trafilatura::{extract_with_options, Options};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchReport {
    pub backend: String,
    pub query: String,
    pub results: Vec<WebSearchResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFetchReport {
    pub url: String,
    pub status: u16,
    pub content_type: String,
    pub mode: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedWebContent {
    pub report: WebFetchReport,
    pub raw_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWebSearchBackend {
    pub kind: SearchBackendKind,
    pub backend: String,
    pub base_url: String,
    api_key: Option<String>,
}

pub fn html_to_markdown(html: &str, url: Option<&str>) -> Result<String, String> {
    extract_markdown(html, url)
}

pub fn prepare_search_backend(
    config: &WebConfig,
    backend_override: Option<SearchBackendKind>,
) -> Result<PreparedWebSearchBackend, String> {
    let effective_kind = backend_override.unwrap_or(config.search.backend);
    if effective_kind == SearchBackendKind::Auto {
        for kind in [
            SearchBackendKind::Kagi,
            SearchBackendKind::Exa,
            SearchBackendKind::Tavily,
            SearchBackendKind::Brave,
        ] {
            let Some(api_key_env) = kind.default_api_key_env() else {
                continue;
            };
            let Ok(api_key) = std::env::var(api_key_env) else {
                continue;
            };
            return Ok(PreparedWebSearchBackend {
                kind,
                backend: search_backend_name(kind).to_string(),
                base_url: kind.default_base_url().to_string(),
                api_key: Some(api_key),
            });
        }

        return Ok(PreparedWebSearchBackend {
            kind: SearchBackendKind::Duckduckgo,
            backend: search_backend_name(SearchBackendKind::Duckduckgo).to_string(),
            base_url: config.search.effective_base_url().to_string(),
            api_key: None,
        });
    }

    let api_key = match backend_override {
        Some(_) => effective_kind
            .default_api_key_env()
            .map(load_api_key)
            .transpose()?,
        None => config
            .search
            .effective_api_key_env()
            .map(load_api_key)
            .transpose()?,
    };
    let base_url = if backend_override.is_some() {
        effective_kind.default_base_url().to_string()
    } else {
        config.search.effective_base_url().to_string()
    };

    Ok(PreparedWebSearchBackend {
        kind: effective_kind,
        backend: search_backend_name(effective_kind).to_string(),
        base_url,
        api_key,
    })
}

#[allow(clippy::too_many_lines)]
pub fn search_web(
    user_agent: &str,
    backend: &PreparedWebSearchBackend,
    query: &str,
    limit: usize,
) -> Result<WebSearchReport, String> {
    let client = build_web_client(user_agent)?;
    let results = match backend.kind {
        SearchBackendKind::Duckduckgo | SearchBackendKind::Auto => {
            let response = client
                .get(&backend.base_url)
                .query(&[("q", query)])
                .send()
                .map_err(|error| error.to_string())?;
            if !response.status().is_success() {
                return Err(format!(
                    "DuckDuckGo search failed with status {}",
                    response.status()
                ));
            }
            let html = response.text().map_err(|error| error.to_string())?;
            let results = parse_duckduckgo_search_results(&html, limit.max(1));
            if results.is_empty() {
                return Err("unexpected DuckDuckGo response shape".to_string());
            }
            results
        }
        SearchBackendKind::Kagi => {
            let limit_value = limit.max(1).to_string();
            let response = client
                .get(&backend.base_url)
                .header(
                    AUTHORIZATION,
                    format!("Bot {}", required_api_key(backend, "Kagi")?),
                )
                .query(&[("q", query), ("limit", limit_value.as_str())])
                .send()
                .map_err(|error| error.to_string())?;
            if !response.status().is_success() {
                return Err(format!(
                    "web search failed with status {}",
                    response.status()
                ));
            }
            let payload = response
                .json::<serde_json::Value>()
                .map_err(|error| error.to_string())?;
            parse_search_results(&payload).ok_or_else(|| {
                "web search backend returned an unexpected payload shape".to_string()
            })?
        }
        SearchBackendKind::Exa => {
            let response = client
                .post(&backend.base_url)
                .header("x-api-key", required_api_key(backend, "Exa")?)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&serde_json::json!({
                    "query": query,
                    "numResults": limit.max(1),
                    "type": "neural",
                    "useAutoprompt": true,
                    "contents": { "text": { "maxCharacters": 500 } }
                }))
                .send()
                .map_err(|error| error.to_string())?;
            if !response.status().is_success() {
                return Err(format!(
                    "Exa search failed with status {}",
                    response.status()
                ));
            }
            let payload = response
                .json::<serde_json::Value>()
                .map_err(|error| error.to_string())?;
            payload
                .get("results")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| "unexpected Exa response shape".to_string())?
                .iter()
                .filter_map(|item| {
                    let title = item
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let url = item.get("url").and_then(serde_json::Value::as_str)?;
                    let snippet = item
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| item.get("snippet").and_then(serde_json::Value::as_str))
                        .unwrap_or_default();
                    Some(WebSearchResult {
                        title: title.to_string(),
                        url: url.to_string(),
                        snippet: snippet.to_string(),
                    })
                })
                .collect()
        }
        SearchBackendKind::Tavily => {
            let response = client
                .post(&backend.base_url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&serde_json::json!({
                    "api_key": required_api_key(backend, "Tavily")?,
                    "query": query,
                    "max_results": limit.max(1),
                    "search_depth": "basic"
                }))
                .send()
                .map_err(|error| error.to_string())?;
            if !response.status().is_success() {
                return Err(format!(
                    "Tavily search failed with status {}",
                    response.status()
                ));
            }
            let payload = response
                .json::<serde_json::Value>()
                .map_err(|error| error.to_string())?;
            payload
                .get("results")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| "unexpected Tavily response shape".to_string())?
                .iter()
                .filter_map(|item| {
                    let title = item
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let url = item.get("url").and_then(serde_json::Value::as_str)?;
                    let snippet = item
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    Some(WebSearchResult {
                        title: title.to_string(),
                        url: url.to_string(),
                        snippet: snippet.to_string(),
                    })
                })
                .collect()
        }
        SearchBackendKind::Brave => {
            let count = limit.clamp(1, 20).to_string();
            let response = client
                .get(&backend.base_url)
                .header("Accept", "application/json")
                .header("Accept-Encoding", "gzip")
                .header("X-Subscription-Token", required_api_key(backend, "Brave")?)
                .query(&[("q", query), ("count", count.as_str())])
                .send()
                .map_err(|error| error.to_string())?;
            if !response.status().is_success() {
                return Err(format!(
                    "Brave search failed with status {}",
                    response.status()
                ));
            }
            let payload = response
                .json::<serde_json::Value>()
                .map_err(|error| error.to_string())?;
            payload
                .get("web")
                .and_then(|web| web.get("results"))
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| "unexpected Brave response shape".to_string())?
                .iter()
                .filter_map(|item| {
                    let title = item
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let url = item.get("url").and_then(serde_json::Value::as_str)?;
                    let snippet = item
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    Some(WebSearchResult {
                        title: title.to_string(),
                        url: url.to_string(),
                        snippet: snippet.to_string(),
                    })
                })
                .collect()
        }
    };

    Ok(WebSearchReport {
        backend: backend.backend.clone(),
        query: query.to_string(),
        results,
    })
}

pub fn fetch_web(config: &WebConfig, url: &str, mode: &str) -> Result<WebFetchReport, String> {
    fetch_web_content(config, url, mode).map(|fetched| fetched.report)
}

pub fn fetch_web_content(
    config: &WebConfig,
    url: &str,
    mode: &str,
) -> Result<FetchedWebContent, String> {
    let client = build_web_client(&config.user_agent)?;
    if !robots_allow_fetch(&client, url, &config.user_agent) {
        return Err("fetch blocked by robots.txt (best-effort check)".to_string());
    }

    let response = client.get(url).send().map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = response.bytes().map_err(|error| error.to_string())?;

    Ok(FetchedWebContent {
        report: WebFetchReport {
            url: url.to_string(),
            status,
            content_type: content_type.clone(),
            mode: mode.to_string(),
            content: render_fetched_content(&bytes, &content_type, url, mode)?,
            saved: None,
        },
        raw_bytes: bytes.to_vec(),
    })
}

fn extract_markdown(html: &str, url: Option<&str>) -> Result<String, String> {
    let options = Options {
        output_markdown: true,
        include_links: true,
        include_tables: true,
        favor_precision: true,
        url: url.map(ToOwned::to_owned),
        ..Options::default()
    };
    let result = extract_with_options(html, &options).map_err(|error| {
        format!(
            "rs-trafilatura extraction failed: {error}; retry with HTML or raw output if you need the original page"
        )
    })?;
    result
        .content_markdown
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .map(normalize_markdown)
        .or_else(|| {
            (!result.content_text.trim().is_empty())
                .then(|| normalize_plain_text(&result.content_text))
        })
        .ok_or_else(|| {
            "rs-trafilatura could not extract readable main content from the fetched HTML; retry with HTML or raw output if you need the original page".to_string()
        })
}

fn normalize_markdown(content: &str) -> String {
    Regex::new(r"\n{3,}")
        .expect("regex should compile")
        .replace_all(content.trim(), "\n\n")
        .into_owned()
}

fn normalize_plain_text(content: &str) -> String {
    Regex::new(r"\n{3,}")
        .expect("regex should compile")
        .replace_all(content.trim(), "\n\n")
        .into_owned()
}

fn load_api_key(env_name: &str) -> Result<String, String> {
    std::env::var(env_name).map_err(|_| format!("missing web search API key env var {env_name}"))
}

fn required_api_key<'a>(
    backend: &'a PreparedWebSearchBackend,
    backend_name: &str,
) -> Result<&'a str, String> {
    backend.api_key.as_deref().ok_or_else(|| {
        format!("configured web search backend `{backend_name}` requires an API key env var")
    })
}

fn search_backend_name(kind: SearchBackendKind) -> &'static str {
    match kind {
        SearchBackendKind::Auto => "auto",
        SearchBackendKind::Duckduckgo => "duckduckgo",
        SearchBackendKind::Kagi => "kagi",
        SearchBackendKind::Exa => "exa",
        SearchBackendKind::Tavily => "tavily",
        SearchBackendKind::Brave => "brave",
    }
}

fn build_web_client(user_agent: &str) -> Result<Client, String> {
    Client::builder()
        .user_agent(user_agent)
        .build()
        .map_err(|error| error.to_string())
}

fn parse_search_results(payload: &serde_json::Value) -> Option<Vec<WebSearchResult>> {
    let results = payload
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| payload.get("results").and_then(serde_json::Value::as_array))?;

    Some(
        results
            .iter()
            .filter_map(|item| {
                let title = item
                    .get("title")
                    .or_else(|| item.get("t"))
                    .and_then(serde_json::Value::as_str)?;
                let url = item
                    .get("url")
                    .or_else(|| item.get("u"))
                    .and_then(serde_json::Value::as_str)?;
                let snippet = item
                    .get("snippet")
                    .or_else(|| item.get("desc"))
                    .or_else(|| item.get("body"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                Some(WebSearchResult {
                    title: title.to_string(),
                    url: url.to_string(),
                    snippet: snippet.to_string(),
                })
            })
            .collect(),
    )
}

fn parse_duckduckgo_search_results(html: &str, limit: usize) -> Vec<WebSearchResult> {
    let title_regex = Regex::new(
        r#"(?is)<a[^>]*class="[^"]*\bresult__a\b[^"]*"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#,
    )
    .expect("regex should compile");
    let snippet_regex = Regex::new(
        r#"(?is)<(?:a|div)[^>]*class="[^"]*\bresult__snippet\b[^"]*"[^>]*>(.*?)</(?:a|div)>"#,
    )
    .expect("regex should compile");
    let snippets = snippet_regex
        .captures_iter(html)
        .filter_map(|captures| {
            captures
                .get(1)
                .map(|value| strip_html_fragment(value.as_str()))
        })
        .collect::<Vec<_>>();

    title_regex
        .captures_iter(html)
        .enumerate()
        .take(limit)
        .filter_map(|(index, captures)| {
            let url = captures.get(1)?.as_str();
            let title = captures.get(2)?.as_str();
            Some(WebSearchResult {
                title: strip_html_fragment(title),
                url: normalize_duckduckgo_result_url(url),
                snippet: snippets.get(index).cloned().unwrap_or_default(),
            })
        })
        .collect()
}

fn strip_html_fragment(fragment: &str) -> String {
    let stripped = Regex::new(r"(?is)<[^>]+>")
        .expect("regex should compile")
        .replace_all(fragment, "")
        .into_owned();
    decode_html_entities(stripped.trim())
}

fn normalize_duckduckgo_result_url(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        if let Some(target) = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "uddg").then(|| value.into_owned()))
        {
            return target;
        }
    }
    if let Some(url) = url.strip_prefix("//") {
        return format!("https://{url}");
    }
    url.to_string()
}

fn render_fetched_content(
    bytes: &[u8],
    content_type: &str,
    url: &str,
    mode: &str,
) -> Result<String, String> {
    let rendered = String::from_utf8_lossy(bytes).to_string();
    match mode {
        "raw" | "html" => Ok(rendered),
        _ => {
            if content_type.contains("html") {
                html_to_markdown(&rendered, Some(url))
            } else {
                Ok(rendered)
            }
        }
    }
}

fn decode_html_entities(input: &str) -> String {
    [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ]
    .into_iter()
    .fold(input.to_string(), |acc, (from, to)| acc.replace(from, to))
}

fn robots_allow_fetch(client: &Client, url: &str, user_agent: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return true;
    };
    let Some(host) = parsed.host_str() else {
        return true;
    };
    let authority = parsed
        .port()
        .map_or_else(|| host.to_string(), |port| format!("{host}:{port}"));
    let robots_url = format!("{}://{authority}/robots.txt", parsed.scheme());
    let Ok(response) = client.get(robots_url).send() else {
        return true;
    };
    if !response.status().is_success() {
        return true;
    }
    let Ok(robots) = response.text() else {
        return true;
    };
    robots_allows_path(&robots, parsed.path(), user_agent)
}

fn robots_allows_path(robots: &str, path: &str, user_agent: &str) -> bool {
    let mut applies = false;
    let normalized_agent = user_agent.to_ascii_lowercase();
    for raw_line in robots.lines() {
        let line = raw_line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();

        if key == "user-agent" {
            let value = value.to_ascii_lowercase();
            applies = value == "*" || normalized_agent.starts_with(&value);
        } else if applies && key == "disallow" && !value.is_empty() && path.starts_with(value) {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::{
        fetch_web_content, html_to_markdown, normalize_duckduckgo_result_url,
        prepare_search_backend,
    };
    use crate::config::{SearchBackendKind, WebConfig, WebSearchConfig};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};
    use std::thread;

    fn with_search_backend_env_cleared<T>(callback: impl FnOnce() -> T) -> T {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock should not be poisoned");
        let vars = [
            "KAGI_API_KEY",
            "EXA_API_KEY",
            "TAVILY_API_KEY",
            "BRAVE_API_KEY",
        ];
        let saved = vars
            .into_iter()
            .map(|var| (var, std::env::var_os(var)))
            .collect::<Vec<_>>();
        for (var, _) in &saved {
            std::env::remove_var(var);
        }

        let result = callback();

        for (var, value) in saved {
            if let Some(value) = value {
                std::env::set_var(var, value);
            } else {
                std::env::remove_var(var);
            }
        }

        result
    }

    #[test]
    fn extracts_main_content_markdown_for_article_pages() {
        let html = r"<!doctype html><html><body>
<nav>skip me</nav>
<article>
  <h1>Release Summary</h1>
  <p>This is a substantial article paragraph with enough detail to cross the extraction confidence threshold and keep the extraction path focused on the main content instead of the surrounding chrome.</p>
</article>
</body></html>";

        let markdown =
            html_to_markdown(html, Some("https://example.com/release")).expect("should extract");
        assert!(markdown.contains("Release Summary"));
        assert!(markdown.contains("substantial article paragraph"));
        assert!(!markdown.contains("skip me"));
    }

    #[test]
    fn strips_page_chrome_when_extracting_docs_content() {
        let html = r"<!doctype html><html><body>
<nav>Site Nav</nav>
<main><h1>Docs</h1><p>Short</p></main>
</body></html>";

        let markdown =
            html_to_markdown(html, Some("https://example.com/docs")).expect("should extract");
        assert!(!markdown.contains("Site Nav"));
        assert!(markdown.contains("Docs"));
        assert!(markdown.contains("Short"));
    }

    #[test]
    fn errors_when_no_readable_main_content_is_found() {
        let html = "<!doctype html><html><body></body></html>";

        let error = html_to_markdown(html, Some("https://example.com/empty"))
            .expect_err("empty pages should not produce markdown");
        assert!(error.contains("could not extract readable main content"));
    }

    #[test]
    fn auto_search_backend_falls_back_to_configured_duckduckgo_endpoint() {
        with_search_backend_env_cleared(|| {
            let config = WebConfig {
                user_agent: "Vulcan Test".to_string(),
                search: WebSearchConfig {
                    backend: SearchBackendKind::Auto,
                    api_key_env: None,
                    base_url: Some("http://127.0.0.1:3456/search".to_string()),
                },
            };

            let prepared = prepare_search_backend(&config, None).expect("backend should prepare");
            assert_eq!(prepared.kind, SearchBackendKind::Duckduckgo);
            assert_eq!(prepared.backend, "duckduckgo");
            assert_eq!(prepared.base_url, "http://127.0.0.1:3456/search");
        });
    }

    #[test]
    fn normalizes_duckduckgo_redirect_urls() {
        let url = "https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdocs%3Fq%3Dtest";
        assert_eq!(
            normalize_duckduckgo_result_url(url),
            "https://example.com/docs?q=test"
        );
    }

    #[test]
    fn fetch_web_content_preserves_raw_bytes_for_cli_saves() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener
            .local_addr()
            .expect("listener should expose a local address");
        let base_url = format!("http://{address}");
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

        let config = WebConfig {
            user_agent: "Vulcan Test".to_string(),
            search: WebSearchConfig {
                backend: SearchBackendKind::Duckduckgo,
                api_key_env: None,
                base_url: None,
            },
        };

        let fetched = fetch_web_content(&config, &format!("{base_url}/raw"), "raw")
            .expect("fetch should succeed");
        handle.join().expect("server thread should finish");

        assert_eq!(fetched.report.status, 200);
        assert_eq!(fetched.report.content_type, "application/octet-stream");
        assert_eq!(fetched.report.mode, "raw");
        assert_eq!(fetched.report.content, "raw-body");
        assert_eq!(fetched.raw_bytes, b"raw-body");
    }
}
