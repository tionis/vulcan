use super::{OutlineApi, OutlineRemoteDocument};
use crate::AppError;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct HttpOutlineClient {
    client: Client,
    base_url: Url,
    token: String,
    max_retries: u32,
    page_size: usize,
}

impl HttpOutlineClient {
    pub fn new(
        base_url: &str,
        token: String,
        timeout: Duration,
        max_retries: u32,
        page_size: usize,
    ) -> Result<Self, AppError> {
        let mut base_url = Url::parse(base_url)
            .map_err(|_| AppError::operation("Outline base_url must be a valid HTTP(S) URL"))?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(AppError::operation(
                "Outline base_url must be an HTTP(S) origin without credentials, query, or fragment",
            ));
        }
        if token.trim().is_empty() {
            return Err(AppError::operation("Outline API token is empty"));
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| {
                AppError::operation(format!("failed to configure Outline client: {error}"))
            })?;
        Ok(Self {
            client,
            base_url,
            token,
            max_retries: max_retries.min(10),
            page_size: page_size.clamp(1, 100),
        })
    }

    fn endpoint(&self, method: &str) -> Result<Url, AppError> {
        self.base_url
            .join(&format!("api/{method}"))
            .map_err(|_| AppError::operation("failed to construct Outline API endpoint"))
    }

    fn post<R: DeserializeOwned>(&self, method: &str, body: &Value) -> Result<R, AppError> {
        self.post_envelope::<ApiEnvelope<R>>(method, body)
            .map(|envelope| envelope.data)
    }

    fn post_envelope<R: DeserializeOwned>(
        &self,
        method: &str,
        body: &Value,
    ) -> Result<R, AppError> {
        let endpoint = self.endpoint(method)?;
        for attempt in 0..=self.max_retries {
            let request = self
                .client
                .post(endpoint.clone())
                .bearer_auth(&self.token)
                .json(body);
            match self.send::<R>(request) {
                Ok(value) => return Ok(value),
                Err(RequestFailure::Retryable(message)) if attempt < self.max_retries => {
                    let delay = 100_u64.saturating_mul(1_u64 << attempt.min(4));
                    thread::sleep(Duration::from_millis(delay));
                    if message.is_empty() {
                        return Err(AppError::operation("Outline request failed"));
                    }
                }
                Err(failure) => return Err(AppError::operation(failure.message())),
            }
        }
        Err(AppError::operation("Outline request exhausted retries"))
    }

    fn send<R: DeserializeOwned>(&self, request: RequestBuilder) -> Result<R, RequestFailure> {
        let response = request.send().map_err(|error| {
            if error.is_timeout() || error.is_connect() {
                RequestFailure::Retryable(
                    "Outline request timed out or could not connect".to_string(),
                )
            } else {
                RequestFailure::Fatal("Outline request failed".to_string())
            }
        })?;
        let status = response.status();
        let bytes = response
            .bytes()
            .map_err(|_| RequestFailure::Fatal("failed to read Outline response".to_string()))?;
        if !status.is_success() {
            let message = sanitized_api_error(status, &bytes);
            return if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                Err(RequestFailure::Retryable(message))
            } else {
                Err(RequestFailure::Fatal(message))
            };
        }
        let value = serde_json::from_slice::<R>(&bytes)
            .map_err(|_| RequestFailure::Fatal("Outline returned malformed JSON".to_string()))?;
        Ok(value)
    }
}

impl OutlineApi for HttpOutlineClient {
    fn list_collection_documents(
        &self,
        collection_id: &str,
    ) -> Result<Vec<OutlineRemoteDocument>, AppError> {
        let mut documents = Vec::new();
        let mut offset = 0_usize;
        loop {
            let page: DocumentPage = self.post_envelope(
                "documents.list",
                &json!({
                    "collectionId": collection_id,
                    "limit": self.page_size,
                    "offset": offset,
                }),
            )?;
            let count = page.documents.len();
            documents.extend(page.documents);
            if count == 0 || offset.saturating_add(count) >= page.pagination.total {
                break;
            }
            offset = offset.saturating_add(count);
        }
        Ok(documents)
    }

    fn document_info(&self, id: &str) -> Result<OutlineRemoteDocument, AppError> {
        self.post("documents.info", &json!({ "id": id }))
    }

    fn create_document(
        &self,
        collection_id: &str,
        parent_document_id: Option<&str>,
        title: &str,
        text: &str,
    ) -> Result<OutlineRemoteDocument, AppError> {
        self.post(
            "documents.create",
            &json!({
                "collectionId": collection_id,
                "parentDocumentId": parent_document_id,
                "title": title,
                "text": text,
                "publish": true,
            }),
        )
    }

    fn update_document(
        &self,
        id: &str,
        title: &str,
        text: &str,
    ) -> Result<OutlineRemoteDocument, AppError> {
        self.post(
            "documents.update",
            &json!({ "id": id, "title": title, "text": text, "publish": true }),
        )
    }

    fn move_document(
        &self,
        id: &str,
        collection_id: &str,
        parent_document_id: Option<&str>,
    ) -> Result<OutlineRemoteDocument, AppError> {
        self.post(
            "documents.move",
            &json!({
                "id": id,
                "collectionId": collection_id,
                "parentDocumentId": parent_document_id,
            }),
        )
    }

    fn archive_document(&self, id: &str) -> Result<OutlineRemoteDocument, AppError> {
        self.post("documents.archive", &json!({ "id": id }))
    }
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
struct DocumentPage {
    #[serde(rename = "data")]
    documents: Vec<OutlineRemoteDocument>,
    pagination: Pagination,
}

#[derive(Debug, Deserialize)]
struct Pagination {
    total: usize,
}

enum RequestFailure {
    Retryable(String),
    Fatal(String),
}

impl RequestFailure {
    fn message(self) -> String {
        match self {
            Self::Retryable(message) | Self::Fatal(message) => message,
        }
    }
}

fn sanitized_api_error(status: StatusCode, bytes: &[u8]) -> String {
    let detail = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|message| {
            message.len() <= 240
                && !message.to_ascii_lowercase().contains("token")
                && !message.to_ascii_lowercase().contains("authorization")
        });
    match detail {
        Some(detail) => format!("Outline API returned {status}: {detail}"),
        None => format!("Outline API returned {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn mock_server(responses: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock listener");
        let address = listener.local_addr().expect("listener address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("mock request");
                let mut buffer = vec![0_u8; 16 * 1024];
                let read = stream.read(&mut buffer).expect("read mock request");
                captured
                    .lock()
                    .expect("request lock")
                    .push(String::from_utf8_lossy(&buffer[..read]).to_string());
                let reason = if status == 200 { "OK" } else { "Error" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("mock response");
            }
        });
        (format!("http://{address}"), requests)
    }

    #[test]
    fn list_uses_bearer_auth_and_paginates() {
        let page_one = r#"{"data":[{"id":"one","title":"One","text":"","collectionId":"c","parentDocumentId":null}],"pagination":{"total":2}}"#;
        let page_two = r#"{"data":[{"id":"two","title":"Two","text":"","collectionId":"c","parentDocumentId":null}],"pagination":{"total":2}}"#;
        let (url, requests) = mock_server(vec![(200, page_one), (200, page_two)]);
        let client =
            HttpOutlineClient::new(&url, "secret".to_string(), Duration::from_secs(2), 0, 1)
                .expect("client");
        let documents = client
            .list_collection_documents("c")
            .expect("document list");
        assert_eq!(documents.len(), 2);
        let requests = requests.lock().expect("request lock");
        assert!(requests
            .iter()
            .all(|request| request.contains("authorization: Bearer secret")
                || request.contains("Authorization: Bearer secret")));
        assert!(requests[0].contains("\"offset\":0"));
        assert!(requests[1].contains("\"offset\":1"));
    }

    #[test]
    fn retries_server_errors_and_sanitizes_authentication_failures() {
        let document = r#"{"data":{"id":"one","title":"One","text":"ok","collectionId":"c","parentDocumentId":null}}"#;
        let (url, requests) =
            mock_server(vec![(503, r#"{"message":"try again"}"#), (200, document)]);
        let client = HttpOutlineClient::new(
            &url,
            "top-secret".to_string(),
            Duration::from_secs(2),
            1,
            100,
        )
        .expect("client");
        assert_eq!(client.document_info("one").expect("retried info").id, "one");
        assert_eq!(requests.lock().expect("request lock").len(), 2);

        let (url, _) = mock_server(vec![(401, r#"{"message":"token top-secret rejected"}"#)]);
        let client = HttpOutlineClient::new(
            &url,
            "top-secret".to_string(),
            Duration::from_secs(2),
            0,
            100,
        )
        .expect("client");
        let error = client
            .document_info("one")
            .expect_err("authentication failure");
        assert!(error.to_string().contains("401"));
        assert!(!error.to_string().contains("top-secret"));
    }
}
