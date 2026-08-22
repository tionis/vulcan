use super::{OutlineApi, OutlineRemoteAttachment, OutlineRemoteDocument};
use crate::AppError;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

#[derive(Clone)]
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
            match Self::send::<R>(request) {
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

    fn send<R: DeserializeOwned>(request: RequestBuilder) -> Result<R, RequestFailure> {
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
        id: &str,
        collection_id: &str,
        parent_document_id: Option<&str>,
        title: &str,
        text: &str,
    ) -> Result<OutlineRemoteDocument, AppError> {
        let result = self.post(
            "documents.create",
            &json!({
                "id": id,
                "collectionId": collection_id,
                "parentDocumentId": parent_document_id,
                "title": title,
                "text": text,
                "publish": true,
            }),
        );
        match result {
            Ok(document) => Ok(document),
            Err(create_error) => self.document_info(id).or(Err(create_error)),
        }
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

    fn upload_attachment(
        &self,
        document_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<OutlineRemoteAttachment, AppError> {
        let upload: AttachmentCreateData = self.post(
            "attachments.create",
            &json!({
                "documentId": document_id,
                "name": name,
                "contentType": content_type,
                "size": bytes.len(),
            }),
        )?;
        if let Some(upload_url) = upload.upload_url {
            let fields = upload.form.unwrap_or_default();
            self.send_attachment_with_retries(|| {
                let mut form = reqwest::blocking::multipart::Form::new();
                for (key, value) in &fields {
                    form = form.text(key.clone(), value.clone());
                }
                let part = reqwest::blocking::multipart::Part::bytes(bytes.to_vec())
                    .file_name(name.to_string())
                    .mime_str(content_type)
                    .map_err(|_| AppError::operation("invalid attachment content type"))?;
                Ok(self
                    .client
                    .post(&upload_url)
                    .multipart(form.part("file", part)))
            })?;
        } else if let Some(url) = upload.url {
            let headers = upload.headers.unwrap_or_default();
            self.send_attachment_with_retries(|| {
                let mut request = self.client.put(&url).body(bytes.to_vec());
                for (name, value) in &headers {
                    request = request.header(name, value);
                }
                Ok(request)
            })?;
        } else {
            return Err(AppError::operation(
                "Outline attachments.create response did not include an upload URL",
            ));
        }
        Ok(OutlineRemoteAttachment {
            id: upload.attachment.id,
            url: upload.attachment.url,
        })
    }
}

impl HttpOutlineClient {
    fn send_attachment_with_retries(
        &self,
        request: impl Fn() -> Result<RequestBuilder, AppError>,
    ) -> Result<(), AppError> {
        for attempt in 0..=self.max_retries {
            match request()?.send() {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response)
                    if attempt < self.max_retries
                        && (response.status() == StatusCode::TOO_MANY_REQUESTS
                            || response.status().is_server_error()) => {}
                Ok(response) => {
                    return Err(AppError::operation(format!(
                        "Outline attachment upload returned {}",
                        response.status()
                    )))
                }
                Err(error)
                    if attempt < self.max_retries && (error.is_timeout() || error.is_connect()) => {
                }
                Err(_) => return Err(AppError::operation("Outline attachment upload failed")),
            }
            let delay = 100_u64.saturating_mul(1_u64 << attempt.min(4));
            thread::sleep(Duration::from_millis(delay));
        }
        Err(AppError::operation(
            "Outline attachment upload exhausted retries",
        ))
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentCreateData {
    attachment: AttachmentData,
    upload_url: Option<String>,
    url: Option<String>,
    form: Option<BTreeMap<String, String>>,
    headers: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct AttachmentData {
    id: String,
    url: String,
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

    #[test]
    fn attachment_create_put_mode_uploads_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock listener");
        let address = listener.local_addr().expect("listener address");
        let handle = std::thread::spawn(move || {
            let (mut create_stream, _) = listener.accept().expect("attachment create request");
            let mut request = vec![0_u8; 16 * 1024];
            let read = create_stream
                .read(&mut request)
                .expect("read create request");
            let create_request = String::from_utf8_lossy(&request[..read]);
            assert!(create_request.contains("/api/attachments.create"));
            assert!(create_request.contains("\"documentId\":\"document\""));
            let body = format!(
                r#"{{"data":{{"attachment":{{"id":"asset","url":"https://outline.test/asset"}},"url":"http://{address}/upload","headers":{{"Content-Type":"image/png"}}}}}}"#
            );
            write!(
                create_stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("create response");

            let (mut upload_stream, _) = listener.accept().expect("attachment upload request");
            let mut upload = vec![0_u8; 16 * 1024];
            let read = upload_stream
                .read(&mut upload)
                .expect("read upload request");
            let upload = &upload[..read];
            assert!(upload.starts_with(b"PUT /upload"));
            assert!(upload.ends_with(b"png bytes"));
            upload_stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("upload response");
        });
        let client = HttpOutlineClient::new(
            &format!("http://{address}"),
            "secret".to_string(),
            Duration::from_secs(2),
            0,
            100,
        )
        .expect("client");
        let attachment = client
            .upload_attachment("document", "logo.png", "image/png", b"png bytes")
            .expect("attachment upload");
        assert_eq!(attachment.id, "asset");
        assert_eq!(attachment.url, "https://outline.test/asset");
        handle.join().expect("mock server");
    }
}
