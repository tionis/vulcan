use super::{
    OutlineApi, OutlineCollectionCreate, OutlineCollectionUpdate, OutlineDownloadedAttachment,
    OutlineRemoteAttachment, OutlineRemoteCollection, OutlineRemoteDocument,
};
use crate::AppError;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Read;
use std::thread;
use std::time::Duration;

#[derive(Clone)]
pub struct HttpOutlineClient {
    client: Client,
    base_url: Url,
    token: String,
    max_retries: u32,
    page_size: usize,
    max_response_bytes: usize,
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
            max_response_bytes: 64 * 1024 * 1024,
        })
    }

    #[must_use]
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes.max(1);
        self
    }

    fn endpoint(&self, method: &str) -> Result<Url, AppError> {
        self.base_url
            .join(&format!("api/{method}"))
            .map_err(|_| AppError::operation("failed to construct Outline API endpoint"))
    }

    #[must_use]
    pub fn connector_identity(&self) -> String {
        self.base_url.to_string()
    }

    fn post<R: DeserializeOwned>(&self, method: &str, body: &Value) -> Result<R, AppError> {
        let envelope = self.post_envelope::<ApiEnvelope<R>>(method, body)?;
        if envelope.ok == Some(false) {
            return Err(AppError::operation(
                "Outline returned an unsuccessful response with a success status",
            ));
        }
        Ok(envelope.data)
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
                Err(RequestFailure::Retryable {
                    message,
                    retry_after,
                }) if attempt < self.max_retries => {
                    thread::sleep(retry_after.unwrap_or_else(|| exponential_backoff(attempt)));
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
        let mut response = request.send().map_err(|error| {
            if error.is_timeout() || error.is_connect() {
                RequestFailure::Retryable {
                    message: "Outline request timed out or could not connect".to_string(),
                    retry_after: None,
                }
            } else {
                RequestFailure::Fatal("Outline request failed".to_string())
            }
        })?;
        let status = response.status();
        let retry_after = retry_after_delay(response.headers());
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > self.max_response_bytes)
        {
            return Err(RequestFailure::Fatal(format!(
                "Outline API response exceeds the {}-byte limit",
                self.max_response_bytes
            )));
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(self.max_response_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| RequestFailure::Fatal("failed to read Outline response".to_string()))?;
        if bytes.len() > self.max_response_bytes {
            return Err(RequestFailure::Fatal(format!(
                "Outline API response exceeds the {}-byte limit",
                self.max_response_bytes
            )));
        }
        if !status.is_success() {
            let message = sanitized_api_error(status, &bytes);
            return if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                Err(RequestFailure::Retryable {
                    message,
                    retry_after,
                })
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
    fn list_collections(
        &self,
        query: Option<&str>,
        archived: bool,
    ) -> Result<Vec<OutlineRemoteCollection>, AppError> {
        let mut collections = Vec::new();
        let mut offset = 0_usize;
        let mut expected_total = None;
        let mut collection_ids = BTreeSet::new();
        loop {
            let mut body = json!({
                "limit": self.page_size,
                "offset": offset,
            });
            if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
                body["query"] = Value::String(query.to_string());
            }
            if archived {
                body["statusFilter"] = json!(["archived"]);
            }
            let page: CollectionPage = self.post_envelope("collections.list", &body)?;
            if page.ok == Some(false) {
                return Err(AppError::operation(
                    "Outline returned an unsuccessful collection listing with a success status",
                ));
            }
            if expected_total
                .replace(page.pagination.total)
                .is_some_and(|total| total != page.pagination.total)
            {
                return Err(AppError::operation(
                    "Outline collections changed while their paginated snapshot was being listed; retry",
                ));
            }
            let count = page.collections.len();
            if page
                .collections
                .iter()
                .any(|collection| !collection_ids.insert(collection.id.clone()))
            {
                return Err(AppError::operation(
                    "Outline returned a duplicate collection while paginating; retry",
                ));
            }
            collections.extend(page.collections);
            if count == 0 || offset.saturating_add(count) >= page.pagination.total {
                break;
            }
            offset = offset.saturating_add(count);
        }
        if collections.len() != expected_total.unwrap_or_default() {
            return Err(AppError::operation(
                "Outline returned an incomplete paginated collection snapshot; retry",
            ));
        }
        Ok(collections)
    }

    fn collection_info(&self, id: &str) -> Result<OutlineRemoteCollection, AppError> {
        self.post("collections.info", &json!({ "id": id }))
    }

    fn create_collection(
        &self,
        request: &OutlineCollectionCreate,
    ) -> Result<OutlineRemoteCollection, AppError> {
        let body = serde_json::to_value(request).map_err(AppError::operation)?;
        self.post("collections.create", &body)
    }

    fn update_collection(
        &self,
        id: &str,
        request: &OutlineCollectionUpdate,
    ) -> Result<OutlineRemoteCollection, AppError> {
        let mut body = serde_json::to_value(request).map_err(AppError::operation)?;
        body["id"] = Value::String(id.to_string());
        self.post("collections.update", &body)
    }

    fn archive_collection(&self, id: &str) -> Result<OutlineRemoteCollection, AppError> {
        self.post("collections.archive", &json!({ "id": id }))
    }

    fn restore_collection(&self, id: &str) -> Result<OutlineRemoteCollection, AppError> {
        self.post("collections.restore", &json!({ "id": id }))
    }

    fn list_collection_documents(
        &self,
        collection_id: &str,
    ) -> Result<Vec<OutlineRemoteDocument>, AppError> {
        let mut documents = Vec::new();
        let mut offset = 0_usize;
        let mut expected_total = None;
        let mut document_ids = BTreeSet::new();
        loop {
            let page: DocumentPage = self.post_envelope(
                "documents.list",
                &json!({
                    "collectionId": collection_id,
                    "limit": self.page_size,
                    "offset": offset,
                }),
            )?;
            if page.ok == Some(false) {
                return Err(AppError::operation(
                    "Outline returned an unsuccessful document listing with a success status",
                ));
            }
            if expected_total
                .replace(page.pagination.total)
                .is_some_and(|total| total != page.pagination.total)
            {
                return Err(AppError::operation(
                    "Outline collection changed while its paginated document snapshot was being listed; retry the pull",
                ));
            }
            let count = page.documents.len();
            if page
                .documents
                .iter()
                .any(|document| !document_ids.insert(document.id.clone()))
            {
                return Err(AppError::operation(
                    "Outline returned a duplicate document while paginating a changing collection; retry the pull",
                ));
            }
            documents.extend(page.documents);
            if count == 0 || offset.saturating_add(count) >= page.pagination.total {
                break;
            }
            offset = offset.saturating_add(count);
        }
        if documents.len() != expected_total.unwrap_or_default() {
            return Err(AppError::operation(
                "Outline returned an incomplete paginated document snapshot; retry the pull",
            ));
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
            let (upload_url, authenticate) = self.attachment_upload_target(&upload_url)?;
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
                let mut request = self
                    .client
                    .post(upload_url.clone())
                    .multipart(form.part("file", part));
                if authenticate {
                    request = request.bearer_auth(&self.token);
                }
                Ok(request)
            })?;
        } else if let Some(url) = upload.url {
            let (url, _) = self.attachment_upload_target(&url)?;
            let headers = upload.headers.unwrap_or_default();
            self.send_attachment_with_retries(|| {
                let mut request = self.client.put(url.clone()).body(bytes.to_vec());
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

    fn download_attachment(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<OutlineDownloadedAttachment, AppError> {
        let url = self.attachment_download_url(url)?;
        for attempt in 0..=self.max_retries {
            let response = self.client.get(url.clone()).bearer_auth(&self.token).send();
            match response {
                Ok(response) if response.status().is_success() => {
                    if response
                        .headers()
                        .get(CONTENT_LENGTH)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<usize>().ok())
                        .is_some_and(|length| length > max_bytes)
                    {
                        return Err(AppError::operation(format!(
                            "Outline attachment exceeds the {max_bytes}-byte download limit"
                        )));
                    }
                    let content_type = response
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    let bytes = response.bytes().map_err(|_| {
                        AppError::operation("failed to read Outline attachment response")
                    })?;
                    if bytes.len() > max_bytes {
                        return Err(AppError::operation(format!(
                            "Outline attachment exceeds the {max_bytes}-byte download limit"
                        )));
                    }
                    return Ok(OutlineDownloadedAttachment {
                        bytes: bytes.to_vec(),
                        content_type,
                    });
                }
                Ok(response)
                    if attempt < self.max_retries
                        && (response.status() == StatusCode::TOO_MANY_REQUESTS
                            || response.status().is_server_error()) =>
                {
                    thread::sleep(
                        retry_after_delay(response.headers())
                            .unwrap_or_else(|| exponential_backoff(attempt)),
                    );
                }
                Ok(response) => {
                    return Err(AppError::operation(format!(
                        "Outline attachment download returned {}",
                        response.status()
                    )))
                }
                Err(error)
                    if attempt < self.max_retries && (error.is_timeout() || error.is_connect()) =>
                {
                    thread::sleep(exponential_backoff(attempt));
                }
                Err(_) => return Err(AppError::operation("Outline attachment download failed")),
            }
        }
        Err(AppError::operation(
            "Outline attachment download exhausted retries",
        ))
    }
}

impl HttpOutlineClient {
    fn attachment_upload_target(&self, target: &str) -> Result<(Url, bool), AppError> {
        let url = Url::parse(target)
            .or_else(|_| self.base_url.join(target))
            .map_err(|_| AppError::operation("Outline attachment upload URL is invalid"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(AppError::operation(
                "Outline attachment upload URL must be HTTP(S) without embedded credentials",
            ));
        }
        let same_origin = url.scheme() == self.base_url.scheme()
            && url.host_str() == self.base_url.host_str()
            && url.port_or_known_default() == self.base_url.port_or_known_default();
        let authenticate = same_origin && url.path().ends_with("/api/files.create");
        Ok((url, authenticate))
    }

    fn attachment_download_url(&self, url: &str) -> Result<Url, AppError> {
        let url = Url::parse(url)
            .or_else(|_| self.base_url.join(url))
            .map_err(|_| AppError::operation("Outline attachment URL is invalid"))?;
        if url.scheme() != self.base_url.scheme()
            || url.host_str() != self.base_url.host_str()
            || url.port_or_known_default() != self.base_url.port_or_known_default()
            || !url.username().is_empty()
            || url.password().is_some()
            || !url.path().starts_with("/api/attachments.redirect")
        {
            return Err(AppError::operation(
                "Outline attachment URL must use the configured origin and redirect endpoint",
            ));
        }
        Ok(url)
    }

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
                            || response.status().is_server_error()) =>
                {
                    thread::sleep(
                        retry_after_delay(response.headers())
                            .unwrap_or_else(|| exponential_backoff(attempt)),
                    );
                    continue;
                }
                Ok(response) => {
                    return Err(AppError::operation(format!(
                        "Outline attachment upload returned {}",
                        response.status()
                    )))
                }
                Err(error)
                    if attempt < self.max_retries && (error.is_timeout() || error.is_connect()) => {
                }
                Err(error) => {
                    let message = if error.is_timeout() {
                        "Outline attachment upload timed out"
                    } else if error.is_connect() {
                        "Outline attachment upload could not connect to its storage target"
                    } else if error.is_builder() {
                        "Outline attachment upload request could not be built"
                    } else if error.is_redirect() {
                        "Outline attachment upload redirect failed"
                    } else if error.is_body() {
                        "Outline attachment upload request body failed"
                    } else {
                        "Outline attachment upload transport failed"
                    };
                    return Err(AppError::operation(message));
                }
            }
            thread::sleep(exponential_backoff(attempt));
        }
        Err(AppError::operation(
            "Outline attachment upload exhausted retries",
        ))
    }
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    data: T,
    #[serde(default)]
    ok: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct DocumentPage {
    #[serde(rename = "data")]
    documents: Vec<OutlineRemoteDocument>,
    pagination: Pagination,
    #[serde(default)]
    ok: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CollectionPage {
    #[serde(rename = "data")]
    collections: Vec<OutlineRemoteCollection>,
    pagination: Pagination,
    #[serde(default)]
    ok: Option<bool>,
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
    Retryable {
        message: String,
        retry_after: Option<Duration>,
    },
    Fatal(String),
}

impl RequestFailure {
    fn message(self) -> String {
        match self {
            Self::Retryable { message, .. } | Self::Fatal(message) => message,
        }
    }
}

fn exponential_backoff(attempt: u32) -> Duration {
    Duration::from_millis(100_u64.saturating_mul(1_u64 << attempt.min(4)))
}

fn retry_after_delay(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds).ok()
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
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    fn mock_server(responses: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        mock_server_with_headers(
            responses
                .into_iter()
                .map(|(status, body)| (status, body, ""))
                .collect(),
        )
    }

    fn mock_server_with_headers(
        responses: Vec<(u16, &'static str, &'static str)>,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock listener");
        let address = listener.local_addr().expect("listener address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        std::thread::spawn(move || {
            for (status, body, headers) in responses {
                let (mut stream, _) = listener.accept().expect("mock request");
                let mut buffer = vec![0_u8; 16 * 1024];
                let read = stream.read(&mut buffer).expect("read mock request");
                captured
                    .lock()
                    .expect("request lock")
                    .push(String::from_utf8_lossy(&buffer[..read]).to_string());
                let reason = if status == 200 { "OK" } else { "Error" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("mock response");
            }
        });
        (format!("http://{address}"), requests)
    }

    fn read_http_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let read = stream.read(&mut chunk).expect("read HTTP request");
            assert!(read > 0, "HTTP request ended before its body was complete");
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            });
            if content_length.is_none_or(|length| request.len() >= header_end + length) {
                return request;
            }
        }
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
    fn collection_lifecycle_uses_typed_outline_endpoints() {
        let collection = r#"{"data":{"id":"11111111-1111-4111-8111-111111111111","name":"Players","description":"Shared lore","url":"/collection/players","urlId":"players","permission":"read_write","sharing":true,"archivedAt":null}}"#;
        let list_one = r#"{"data":[{"id":"11111111-1111-4111-8111-111111111111","name":"Players","url":"/collection/players","urlId":"players"}],"pagination":{"total":2}}"#;
        let list_two = r#"{"data":[{"id":"22222222-2222-4222-8222-222222222222","name":"Lore","url":"/collection/lore","urlId":"lore"}],"pagination":{"total":2}}"#;
        let (url, requests) = mock_server(vec![
            (200, list_one),
            (200, list_two),
            (200, collection),
            (200, collection),
            (200, collection),
            (200, collection),
            (200, collection),
        ]);
        let client =
            HttpOutlineClient::new(&url, "secret".to_string(), Duration::from_secs(2), 0, 1)
                .expect("client");

        assert_eq!(
            client
                .list_collections(Some("play"), false)
                .expect("collections")
                .len(),
            2
        );
        client
            .collection_info("11111111-1111-4111-8111-111111111111")
            .expect("collection info");
        client
            .create_collection(&OutlineCollectionCreate {
                name: "Players".to_string(),
                description: Some("Shared lore".to_string()),
                ..OutlineCollectionCreate::default()
            })
            .expect("collection create");
        client
            .update_collection(
                "11111111-1111-4111-8111-111111111111",
                &OutlineCollectionUpdate {
                    description: Some(Value::Null),
                    sharing: Some(false),
                    ..OutlineCollectionUpdate::default()
                },
            )
            .expect("collection update");
        client
            .archive_collection("11111111-1111-4111-8111-111111111111")
            .expect("collection archive");
        client
            .restore_collection("11111111-1111-4111-8111-111111111111")
            .expect("collection restore");

        let requests = requests.lock().expect("request lock");
        assert!(requests[0].starts_with("POST /api/collections.list "));
        assert!(requests[0].contains("\"query\":\"play\""));
        assert!(requests[2].starts_with("POST /api/collections.info "));
        assert!(requests[3].starts_with("POST /api/collections.create "));
        assert!(requests[3].contains("\"description\":\"Shared lore\""));
        assert!(requests[4].starts_with("POST /api/collections.update "));
        assert!(requests[4].contains("\"description\":null"));
        assert!(requests[4].contains("\"sharing\":false"));
        assert!(requests[5].starts_with("POST /api/collections.archive "));
        assert!(requests[6].starts_with("POST /api/collections.restore "));
    }

    #[test]
    fn list_rejects_a_collection_that_changes_between_pages() {
        let page_one = r#"{"data":[{"id":"one","title":"One","text":"","collectionId":"c","parentDocumentId":null}],"pagination":{"total":2}}"#;
        let page_two = r#"{"data":[{"id":"two","title":"Two","text":"","collectionId":"c","parentDocumentId":null}],"pagination":{"total":3}}"#;
        let (url, _) = mock_server(vec![(200, page_one), (200, page_two)]);
        let client =
            HttpOutlineClient::new(&url, "secret".to_string(), Duration::from_secs(2), 0, 1)
                .expect("client");
        let error = client
            .list_collection_documents("c")
            .expect_err("changing pagination must fail closed");
        assert!(error.to_string().contains("changed while"));
    }

    #[test]
    fn list_rejects_unsuccessful_or_incomplete_success_payloads() {
        let unsuccessful = r#"{"ok":false,"data":[],"pagination":{"total":0}}"#;
        let (url, _) = mock_server(vec![(200, unsuccessful)]);
        let client =
            HttpOutlineClient::new(&url, "secret".to_string(), Duration::from_secs(2), 0, 100)
                .expect("client");
        assert!(client
            .list_collection_documents("c")
            .expect_err("ok=false must fail")
            .to_string()
            .contains("unsuccessful"));

        let missing_markdown = r#"{"data":[{"id":"one","title":"One","collectionId":"c","parentDocumentId":null}],"pagination":{"total":1}}"#;
        let (url, _) = mock_server(vec![(200, missing_markdown)]);
        let client =
            HttpOutlineClient::new(&url, "secret".to_string(), Duration::from_secs(2), 0, 100)
                .expect("client");
        assert!(client
            .list_collection_documents("c")
            .expect_err("missing Markdown body must fail")
            .to_string()
            .contains("malformed JSON"));
    }

    #[test]
    fn document_info_preserves_remote_revision_metadata() {
        let document = r#"{"data":{"id":"one","title":"One","text":"ok","collectionId":"c","parentDocumentId":null,"revision":9,"updatedAt":"2026-08-24T12:00:00Z"}}"#;
        let (url, _) = mock_server(vec![(200, document)]);
        let client =
            HttpOutlineClient::new(&url, "secret".to_string(), Duration::from_secs(2), 0, 100)
                .expect("client");
        let document = client.document_info("one").expect("document info");
        assert_eq!(document.revision, Some(9));
        assert_eq!(document.updated_at.as_deref(), Some("2026-08-24T12:00:00Z"));
    }

    #[test]
    fn api_responses_are_bounded_before_json_parsing() {
        let body = r#"{"data":{"id":"one","title":"One","text":"oversized","collectionId":"c","parentDocumentId":null}}"#;
        let (url, _) = mock_server(vec![(200, body)]);
        let client =
            HttpOutlineClient::new(&url, "secret".to_string(), Duration::from_secs(2), 0, 100)
                .expect("client")
                .with_max_response_bytes(32);
        let error = client
            .document_info("one")
            .expect_err("oversized API response");
        assert!(error.to_string().contains("32-byte limit"));
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
    fn rate_limits_honor_retry_after_before_retrying() {
        let document = r#"{"data":{"id":"one","title":"One","text":"ok","collectionId":"c","parentDocumentId":null}}"#;
        let (url, requests) = mock_server_with_headers(vec![
            (
                429,
                r#"{"message":"rate limit exceeded"}"#,
                "Retry-After: 0.001\r\n",
            ),
            (200, document, ""),
        ]);
        let client =
            HttpOutlineClient::new(&url, "secret".to_string(), Duration::from_secs(2), 1, 100)
                .expect("client");

        assert_eq!(client.document_info("one").expect("retried info").id, "one");
        assert_eq!(requests.lock().expect("request lock").len(), 2);
    }

    #[test]
    fn retry_after_accepts_outline_fractional_seconds_and_rejects_invalid_values() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(RETRY_AFTER, "59.693".parse().expect("header"));
        assert_eq!(
            retry_after_delay(&headers),
            Some(Duration::from_millis(59_693))
        );
        headers.insert(RETRY_AFTER, "not-a-delay".parse().expect("header"));
        assert_eq!(retry_after_delay(&headers), None);
        headers.insert(RETRY_AFTER, "1e300".parse().expect("header"));
        assert_eq!(retry_after_delay(&headers), None);
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

    #[test]
    fn attachment_create_resolves_and_authenticates_local_storage_uploads() {
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
            let body = r#"{"data":{"attachment":{"id":"asset","url":"/api/attachments.redirect?id=asset"},"uploadUrl":"/api/files.create","form":{"key":"uploads/asset.png"}}}"#;
            write!(
                create_stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("create response");

            let (mut upload_stream, _) = listener.accept().expect("attachment upload request");
            let upload = read_http_request(&mut upload_stream);
            let upload = String::from_utf8_lossy(&upload);
            assert!(upload.starts_with("POST /api/files.create"));
            assert!(
                upload.contains("authorization: Bearer secret")
                    || upload.contains("Authorization: Bearer secret")
            );
            assert!(upload.contains("name=\"key\""));
            assert!(upload.contains("uploads/asset.png"));
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

        client
            .upload_attachment("document", "asset.png", "image/png", b"png bytes")
            .expect("local attachment upload");
        handle.join().expect("mock server");
    }

    #[test]
    fn attachment_upload_credentials_are_limited_to_outline_files_endpoint() {
        let client = HttpOutlineClient::new(
            "https://outline.example.test",
            "secret".to_string(),
            Duration::from_secs(2),
            0,
            100,
        )
        .expect("client");

        let (local, authenticate) = client
            .attachment_upload_target("/api/files.create")
            .expect("local target");
        assert_eq!(
            local.as_str(),
            "https://outline.example.test/api/files.create"
        );
        assert!(authenticate);

        let (_, authenticate) = client
            .attachment_upload_target("https://storage.example.test/upload?signature=secret")
            .expect("external target");
        assert!(!authenticate);
        let (_, authenticate) = client
            .attachment_upload_target("https://outline.example.test/custom-upload")
            .expect("custom same-origin target");
        assert!(!authenticate);
        assert!(client
            .attachment_upload_target("https://user:pass@storage.example.test/upload")
            .is_err());
    }

    #[test]
    fn attachment_download_is_authenticated_origin_bounded_and_size_limited() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock listener");
        let address = listener.local_addr().expect("listener address");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("attachment request");
            let mut request = vec![0_u8; 16 * 1024];
            let read = stream.read(&mut request).expect("read attachment request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("/api/attachments.redirect?id=asset"));
            assert!(request.contains("Bearer secret"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 3\r\nConnection: close\r\n\r\npng",
                )
                .expect("attachment response");
        });
        let base_url = format!("http://{address}");
        let client = HttpOutlineClient::new(
            &base_url,
            "secret".to_string(),
            Duration::from_secs(2),
            0,
            100,
        )
        .expect("client");
        let attachment = client
            .download_attachment("/api/attachments.redirect?id=asset", 3)
            .expect("attachment download");
        assert_eq!(attachment.bytes, b"png");
        assert_eq!(attachment.content_type.as_deref(), Some("image/png"));
        assert!(client
            .download_attachment("https://example.test/api/attachments.redirect?id=asset", 3)
            .is_err());
        handle.join().expect("mock server");

        let (base_url, _) = mock_server(vec![(200, "oversized")]);
        let client = HttpOutlineClient::new(
            &base_url,
            "secret".to_string(),
            Duration::from_secs(2),
            0,
            100,
        )
        .expect("client");
        assert!(client
            .download_attachment("/api/attachments.redirect?id=asset", 3)
            .is_err());
    }
}
