use crate::provider::{
    EmbeddingError, EmbeddingInput, EmbeddingProvider, EmbeddingResult, ModelMetadata,
};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const DEFAULT_MAX_BATCH_SIZE: usize = 32;
const DEFAULT_MAX_CONCURRENCY: usize = 4;
const DEFAULT_MAX_INPUT_TOKENS: usize = 8_192;
const DEFAULT_MAX_RETRIES: usize = 3;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;
const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 100;

#[derive(Debug, Clone)]
pub struct OpenAICompatibleConfig {
    pub provider_name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model_name: String,
    pub normalized: bool,
    pub max_batch_size: usize,
    pub max_input_tokens: usize,
    pub max_concurrency: usize,
    pub max_retries: usize,
    pub request_timeout: Duration,
    pub retry_base_delay: Duration,
}

impl Default for OpenAICompatibleConfig {
    fn default() -> Self {
        Self {
            provider_name: "openai-compatible".to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            api_key: None,
            model_name: "text-embedding-3-small".to_string(),
            normalized: true,
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            max_input_tokens: DEFAULT_MAX_INPUT_TOKENS,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            max_retries: DEFAULT_MAX_RETRIES,
            request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
            retry_base_delay: Duration::from_millis(DEFAULT_RETRY_BASE_DELAY_MS),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenAICompatibleProvider {
    client: Client,
    endpoint_url: String,
    api_key: Option<String>,
    metadata: Arc<Mutex<ModelMetadata>>,
    max_concurrency: usize,
    max_retries: usize,
    retry_base_delay: Duration,
}

impl OpenAICompatibleProvider {
    pub fn new(config: OpenAICompatibleConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(|error| format!("failed to build embeddings HTTP client: {error}"))?;

        Ok(Self {
            client,
            endpoint_url: format!("{}/embeddings", config.base_url.trim_end_matches('/')),
            api_key: config.api_key,
            metadata: Arc::new(Mutex::new(ModelMetadata {
                provider_name: config.provider_name,
                model_name: config.model_name,
                dimensions: 0,
                normalized: config.normalized,
                max_batch_size: config.max_batch_size.max(1),
                max_input_tokens: config.max_input_tokens.max(1),
            })),
            max_concurrency: config.max_concurrency.max(1),
            max_retries: config.max_retries.max(1),
            retry_base_delay: config.retry_base_delay,
        })
    }
}

impl EmbeddingProvider for OpenAICompatibleProvider {
    fn metadata(&self) -> ModelMetadata {
        self.metadata
            .lock()
            .expect("provider metadata mutex should not be poisoned")
            .clone()
    }

    fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Vec<EmbeddingResult> {
        if inputs.is_empty() {
            return Vec::new();
        }

        let batch_size = self.metadata().max_batch_size;
        let batches = inputs.chunks(batch_size).collect::<Vec<_>>();
        let mut completed_batches = Vec::with_capacity(batches.len());

        for batch_group in batches.chunks(self.max_concurrency) {
            thread::scope(|scope| {
                let mut handles = Vec::with_capacity(batch_group.len());

                for (group_index, batch) in batch_group.iter().enumerate() {
                    let client = self.client.clone();
                    let endpoint_url = self.endpoint_url.clone();
                    let api_key = self.api_key.clone();
                    let model_name = self.metadata().model_name;
                    let max_retries = self.max_retries;
                    let retry_base_delay = self.retry_base_delay;

                    handles.push(scope.spawn(move || {
                        let texts = batch
                            .iter()
                            .map(|input| input.text.clone())
                            .collect::<Vec<_>>();
                        let results = request_embeddings(
                            &client,
                            &endpoint_url,
                            api_key.as_deref(),
                            &model_name,
                            &texts,
                            max_retries,
                            retry_base_delay,
                        );
                        (group_index, results)
                    }));
                }

                let mut group_results = handles
                    .into_iter()
                    .map(|handle| {
                        handle
                            .join()
                            .expect("embedding worker thread should complete")
                    })
                    .collect::<Vec<_>>();
                group_results.sort_by_key(|(group_index, _)| *group_index);
                completed_batches.extend(group_results.into_iter().map(|(_, results)| results));
            });
        }

        let dimensions = completed_batches
            .iter()
            .flat_map(|results| results.iter())
            .find_map(|result| result.as_ref().ok().map(Vec::len));
        if let Some(dimensions) = dimensions {
            let mut metadata = self
                .metadata
                .lock()
                .expect("provider metadata mutex should not be poisoned");
            metadata.dimensions = dimensions;
        }

        completed_batches.into_iter().flatten().collect()
    }
}

fn request_embeddings(
    client: &Client,
    endpoint_url: &str,
    api_key: Option<&str>,
    model_name: &str,
    inputs: &[String],
    max_retries: usize,
    retry_base_delay: Duration,
) -> Vec<EmbeddingResult> {
    let mut attempt = 0_usize;

    loop {
        match execute_embedding_request(client, endpoint_url, api_key, model_name, inputs) {
            Ok(results) => return results,
            Err(error) if error.retryable && attempt + 1 < max_retries => {
                thread::sleep(backoff_delay(retry_base_delay, attempt));
                attempt += 1;
            }
            Err(error) => return vec![Err(error); inputs.len()],
        }
    }
}

fn execute_embedding_request(
    client: &Client,
    endpoint_url: &str,
    api_key: Option<&str>,
    model_name: &str,
    inputs: &[String],
) -> Result<Vec<EmbeddingResult>, EmbeddingError> {
    let mut request = client
        .post(endpoint_url)
        .header(CONTENT_TYPE, "application/json");
    if let Some(api_key) = api_key {
        request = request.header(AUTHORIZATION, format!("Bearer {api_key}"));
    }

    let response = request
        .json(&EmbeddingsRequest {
            model: model_name,
            input: inputs,
        })
        .send()
        .map_err(|error| classify_transport_error(&error))?;

    let status = response.status();
    if !status.is_success() {
        let retryable = status.as_u16() == 429 || status.is_server_error();
        let message = response.text().unwrap_or_else(|_| String::new());
        let error_message = if message.trim().is_empty() {
            format!("embeddings request failed with HTTP {}", status.as_u16())
        } else {
            format!(
                "embeddings request failed with HTTP {}: {message}",
                status.as_u16()
            )
        };
        return Err(if retryable {
            EmbeddingError::retryable(error_message, Some(status.as_u16()))
        } else {
            EmbeddingError {
                message: error_message,
                retryable: false,
                status_code: Some(status.as_u16()),
            }
        });
    }

    let parsed = response.json::<EmbeddingsResponse>().map_err(|error| {
        EmbeddingError::new(format!(
            "failed to decode embeddings response JSON: {error}"
        ))
    })?;

    if parsed.data.len() != inputs.len() {
        return Err(EmbeddingError::new(format!(
            "embeddings response returned {} vectors for {} inputs",
            parsed.data.len(),
            inputs.len()
        )));
    }

    let mut ordered = parsed.data;
    ordered.sort_by_key(|row| row.index);
    Ok(ordered
        .into_iter()
        .map(|row| Ok(row.embedding))
        .collect::<Vec<_>>())
}

fn classify_transport_error(error: &reqwest::Error) -> EmbeddingError {
    let retryable = error.is_timeout() || error.is_connect();
    if retryable {
        EmbeddingError::retryable(format!("embeddings request failed: {error}"), None)
    } else {
        EmbeddingError::new(format!("embeddings request failed: {error}"))
    }
}

fn backoff_delay(base_delay: Duration, attempt: usize) -> Duration {
    let shift = u32::try_from(attempt).unwrap_or(u32::MAX).min(10);
    let factor = 2_u32.saturating_pow(shift);
    base_delay.saturating_mul(factor)
}

#[derive(Debug, Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingResponseRow>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponseRow {
    index: usize,
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use ulid::Ulid;

    #[test]
    fn provider_batches_requests_and_learns_dimensions() {
        let server = MockServer::spawn(
            vec![
                MockResponse::json(
                    200,
                    r#"{"data":[{"index":1,"embedding":[0.0,1.0,0.0]},{"index":0,"embedding":[1.0,0.0,0.0]}]}"#,
                ),
                MockResponse::json(200, r#"{"data":[{"index":0,"embedding":[0.0,0.0,1.0]}]}"#),
            ],
            2,
        );
        let provider = OpenAICompatibleProvider::new(OpenAICompatibleConfig {
            base_url: server.base_url(),
            max_batch_size: 2,
            max_concurrency: 1,
            retry_base_delay: Duration::from_millis(1),
            ..OpenAICompatibleConfig::default()
        })
        .expect("provider should build");

        let results = provider.embed_batch(&[
            EmbeddingInput {
                id: Ulid::new(),
                text: "alpha".to_string(),
            },
            EmbeddingInput {
                id: Ulid::new(),
                text: "beta".to_string(),
            },
            EmbeddingInput {
                id: Ulid::new(),
                text: "gamma".to_string(),
            },
        ]);

        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0].as_ref().expect("first embedding should succeed"),
            &vec![1.0, 0.0, 0.0]
        );
        assert_eq!(
            results[1]
                .as_ref()
                .expect("second embedding should succeed"),
            &vec![0.0, 1.0, 0.0]
        );
        assert_eq!(
            results[2].as_ref().expect("third embedding should succeed"),
            &vec![0.0, 0.0, 1.0]
        );
        assert_eq!(provider.metadata().dimensions, 3);

        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        assert_eq!(request_inputs(&requests[0]), vec!["alpha", "beta"]);
        assert_eq!(request_inputs(&requests[1]), vec!["gamma"]);
    }

    #[test]
    fn provider_retries_transient_errors_and_sends_auth_header() {
        let server = MockServer::spawn(
            vec![
                MockResponse::text(500, "temporary failure"),
                MockResponse::json(200, r#"{"data":[{"index":0,"embedding":[0.5,0.5]}]}"#),
            ],
            2,
        );
        let provider = OpenAICompatibleProvider::new(OpenAICompatibleConfig {
            base_url: server.base_url(),
            api_key: Some("secret".to_string()),
            max_batch_size: 1,
            max_concurrency: 1,
            max_retries: 2,
            retry_base_delay: Duration::from_millis(1),
            ..OpenAICompatibleConfig::default()
        })
        .expect("provider should build");

        let results = provider.embed_batch(&[EmbeddingInput {
            id: Ulid::new(),
            text: "retry me".to_string(),
        }]);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0]
                .as_ref()
                .expect("embedding should succeed after retry"),
            &vec![0.5, 0.5]
        );

        let requests = server.finish();
        assert_eq!(requests.len(), 2);
        for request in requests {
            assert!(request
                .headers
                .iter()
                .any(|header| header == "authorization: bearer secret"));
        }
    }

    fn request_inputs(request: &CapturedRequest) -> Vec<String> {
        request
            .body
            .get("input")
            .and_then(Value::as_array)
            .expect("request should include an input array")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("input values should be strings")
                    .to_string()
            })
            .collect()
    }

    #[derive(Debug)]
    struct MockServer {
        base_url: String,
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl MockServer {
        fn spawn(responses: Vec<MockResponse>, expected_requests: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
            let address = listener
                .local_addr()
                .expect("listener should expose local address");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let requests_for_thread = Arc::clone(&requests);
            let response_queue = Arc::new(Mutex::new(VecDeque::from(responses)));
            let response_queue_for_thread = Arc::clone(&response_queue);

            let handle = thread::spawn(move || {
                for _ in 0..expected_requests {
                    let (mut stream, _) = listener.accept().expect("connection should accept");
                    let request = read_request(&mut stream);
                    requests_for_thread
                        .lock()
                        .expect("request log mutex should not be poisoned")
                        .push(request);
                    let response = response_queue_for_thread
                        .lock()
                        .expect("response queue mutex should not be poisoned")
                        .pop_front()
                        .expect("mock response should exist");
                    stream
                        .write_all(response.as_bytes().as_slice())
                        .expect("response should write");
                }
            });

            Self {
                base_url: format!("http://{address}/v1"),
                requests,
                handle: Some(handle),
            }
        }

        fn base_url(&self) -> String {
            self.base_url.clone()
        }

        fn finish(mut self) -> Vec<CapturedRequest> {
            if let Some(handle) = self.handle.take() {
                handle.join().expect("mock server should join cleanly");
            }

            self.requests
                .lock()
                .expect("request log mutex should not be poisoned")
                .clone()
        }
    }

    #[derive(Debug, Clone)]
    struct MockResponse {
        status_code: u16,
        content_type: &'static str,
        body: String,
    }

    impl MockResponse {
        fn json(status_code: u16, body: &str) -> Self {
            Self {
                status_code,
                content_type: "application/json",
                body: body.to_string(),
            }
        }

        fn text(status_code: u16, body: &str) -> Self {
            Self {
                status_code,
                content_type: "text/plain",
                body: body.to_string(),
            }
        }

        fn as_bytes(&self) -> Vec<u8> {
            format!(
                "HTTP/1.1 {} OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                self.status_code,
                self.content_type,
                self.body.len(),
                self.body
            )
            .into_bytes()
        }
    }

    #[derive(Debug, Clone)]
    struct CapturedRequest {
        headers: Vec<String>,
        body: Value,
    }

    fn read_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
        let mut buffer = Vec::new();
        let mut header_end = None;

        loop {
            let mut chunk = [0_u8; 1024];
            let bytes_read = stream.read(&mut chunk).expect("request should be readable");
            if bytes_read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if let Some(position) = find_subslice(&buffer, b"\r\n\r\n") {
                header_end = Some(position + 4);
                break;
            }
        }

        let header_end = header_end.expect("request should contain HTTP headers");
        let header_bytes = &buffer[..header_end];
        let header_text = String::from_utf8(header_bytes.to_vec()).expect("headers should be utf8");
        let headers = header_text
            .lines()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        let content_length = headers
            .iter()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .expect("request should include Content-Length");

        let mut body_bytes = buffer[header_end..].to_vec();
        while body_bytes.len() < content_length {
            let mut chunk = vec![0_u8; content_length - body_bytes.len()];
            let bytes_read = stream
                .read(chunk.as_mut_slice())
                .expect("body should be readable");
            if bytes_read == 0 {
                break;
            }
            body_bytes.extend_from_slice(&chunk[..bytes_read]);
        }

        CapturedRequest {
            headers,
            body: serde_json::from_slice(&body_bytes).expect("request body should be JSON"),
        }
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
