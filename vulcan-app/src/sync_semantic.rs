//! The semantic agent: a provider that groups changed paths into commits and
//! writes their messages without changing any bytes. Batched auto-commits use
//! it when a vault asks for agent messages and the device consents.

use crate::sync::SyncCancellationToken;
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[cfg(feature = "web")]
use std::io::Read;

const MAX_SEMANTIC_AGENT_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_SEMANTIC_AGENT_LABEL_BYTES: usize = 256;
#[cfg(feature = "web")]
const MAX_SEMANTIC_AGENT_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentIdentity {
    pub provider: String,
    pub model: String,
    pub prompt_contract_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentChange {
    pub path: String,
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentRequest {
    pub source_revision: String,
    pub target_revision: String,
    pub changes: Vec<SemanticAgentChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentCommit {
    pub group: String,
    pub message: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAgentOutput {
    pub commits: Vec<SemanticAgentCommit>,
}

pub trait SemanticAgentProvider: Send + Sync {
    fn identity(&self) -> SemanticAgentIdentity;

    fn network_endpoint(&self) -> Option<&str> {
        None
    }

    fn propose(
        &self,
        request: &SemanticAgentRequest,
        cancellation: &SyncCancellationToken,
    ) -> Result<SemanticAgentOutput, AppError>;
}

#[cfg(feature = "web")]
pub struct OpenAiCompatibleSemanticProvider {
    client: reqwest::blocking::Client,
    endpoint: reqwest::Url,
    model: String,
    api_key: Option<String>,
}

#[cfg(feature = "web")]
impl OpenAiCompatibleSemanticProvider {
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<Self, AppError> {
        let mut endpoint = reqwest::Url::parse(base_url).map_err(AppError::operation)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(AppError::operation(
                "semantic agent base URL must be an absolute HTTP(S) URL without credentials, query, or fragment",
            ));
        }
        crate::credential_transport::validate_credential_transport(
            &endpoint,
            api_key.is_some(),
            "semantic agent",
        )?;
        let path = endpoint.path().trim_end_matches('/');
        endpoint.set_path(&format!("{path}/chat/completions"));
        let model = model.into();
        validate_agent_identity(&SemanticAgentIdentity {
            provider: "openai-compatible".to_string(),
            model: model.clone(),
            prompt_contract_version: 1,
        })?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(AppError::operation)?;
        Ok(Self {
            client,
            endpoint,
            model,
            api_key,
        })
    }

    /// Sends `body` on a thread of its own and waits for the answer, giving
    /// up as soon as `cancellation` fires: a blocking HTTP request cannot be
    /// interrupted, so a cancelled request is abandoned to its own timeout.
    fn send(
        &self,
        body: &serde_json::Value,
        cancellation: &SyncCancellationToken,
    ) -> Result<Vec<u8>, AppError> {
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let api_key = self.api_key.clone();
        let body = body.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(send_semantic_request(
                &client,
                endpoint,
                api_key.as_deref(),
                &body,
            ));
        });
        loop {
            match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(result) => return result,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    semantic_cancellation_check(cancellation)?;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(AppError::operation(
                        "the semantic agent request ended without an answer",
                    ));
                }
            }
        }
    }
}

#[cfg(feature = "web")]
fn send_semantic_request(
    client: &reqwest::blocking::Client,
    endpoint: reqwest::Url,
    api_key: Option<&str>,
    body: &serde_json::Value,
) -> Result<Vec<u8>, AppError> {
    let mut builder = client.post(endpoint).json(body);
    if let Some(api_key) = api_key {
        builder = builder.bearer_auth(api_key);
    }
    let mut response = builder.send().map_err(AppError::operation)?;
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take((MAX_SEMANTIC_AGENT_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(AppError::operation)?;
    if bytes.len() > MAX_SEMANTIC_AGENT_RESPONSE_BYTES {
        return Err(AppError::operation(
            "semantic agent response exceeds its byte limit",
        ));
    }
    if !status.is_success() {
        return Err(AppError::operation(format!(
            "semantic agent provider returned HTTP {status}"
        )));
    }
    Ok(bytes)
}

#[cfg(feature = "web")]
impl SemanticAgentProvider for OpenAiCompatibleSemanticProvider {
    fn identity(&self) -> SemanticAgentIdentity {
        SemanticAgentIdentity {
            provider: "openai-compatible".to_string(),
            model: self.model.clone(),
            prompt_contract_version: 1,
        }
    }

    fn network_endpoint(&self) -> Option<&str> {
        Some(self.endpoint.as_str())
    }

    fn propose(
        &self,
        request: &SemanticAgentRequest,
        cancellation: &SyncCancellationToken,
    ) -> Result<SemanticAgentOutput, AppError> {
        semantic_cancellation_check(cancellation)?;
        let input = serde_json::to_string(request).map_err(AppError::operation)?;
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0,
            "response_format": { "type": "json_object" },
            "messages": [
                {
                    "role": "system",
                    "content": "Organize only the supplied accepted Git changes into an ordered semantic commit plan. Return exactly one JSON object with commits, an ordered array of objects containing group (a unique short label), message (a human commit message without Vulcan-Semantic trailers), and paths (an array). Include every supplied path exactly once, name no other path, preserve dependency order, and do not propose or invent file content. Emit no Markdown fence or commentary outside the JSON object."
                },
                { "role": "user", "content": input }
            ]
        });
        let bytes = self.send(&body, cancellation)?;
        semantic_cancellation_check(cancellation)?;
        parse_openai_semantic_output(&bytes)
    }
}

#[cfg(feature = "web")]
fn parse_openai_semantic_output(bytes: &[u8]) -> Result<SemanticAgentOutput, AppError> {
    #[derive(Deserialize)]
    struct Response {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: Message,
    }
    #[derive(Deserialize)]
    struct Message {
        content: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Output {
        commits: Vec<Commit>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Commit {
        group: String,
        message: String,
        paths: Vec<String>,
    }

    let response: Response = serde_json::from_slice(bytes).map_err(AppError::operation)?;
    let content = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| AppError::operation("semantic agent response contained no choices"))?
        .message
        .content;
    let output: Output = serde_json::from_str(&content).map_err(|error| {
        AppError::operation(format!(
            "semantic agent response content was not exact JSON: {error}"
        ))
    })?;
    Ok(SemanticAgentOutput {
        commits: output
            .commits
            .into_iter()
            .map(|commit| SemanticAgentCommit {
                group: commit.group,
                message: commit.message,
                paths: commit.paths,
            })
            .collect(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedSemanticGroup {
    group: String,
    message: String,
    paths: Vec<String>,
}

/// Checks an agent's grouping of `changed_paths`: every path exactly once, no other path, bounded labels,
/// and messages without reserved trailers. Paths come back sorted within each
/// commit and messages trimmed; commit order is the agent's.
pub fn validate_semantic_agent_commits(
    identity: &SemanticAgentIdentity,
    changed_paths: &[String],
    output: SemanticAgentOutput,
) -> Result<Vec<SemanticAgentCommit>, AppError> {
    validate_agent_identity(identity)?;
    Ok(validate_agent_output(changed_paths, output)?
        .into_iter()
        .map(|group| SemanticAgentCommit {
            group: group.group,
            message: group.message,
            paths: group.paths,
        })
        .collect())
}

fn validate_agent_identity(identity: &SemanticAgentIdentity) -> Result<(), AppError> {
    for (field, value) in [
        ("provider", identity.provider.as_str()),
        ("model", identity.model.as_str()),
    ] {
        if value.trim().is_empty()
            || value.len() > MAX_SEMANTIC_AGENT_LABEL_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(AppError::operation(format!(
                "semantic agent {field} identity is invalid"
            )));
        }
    }
    if identity.prompt_contract_version == 0 {
        return Err(AppError::operation(
            "semantic agent prompt contract version must be greater than zero",
        ));
    }
    Ok(())
}

fn validate_agent_output(
    changed_paths: &[String],
    output: SemanticAgentOutput,
) -> Result<Vec<PlannedSemanticGroup>, AppError> {
    if output.commits.is_empty() || output.commits.len() > changed_paths.len() {
        return Err(AppError::operation(
            "semantic agent must propose between one commit and the number of changed paths",
        ));
    }
    let expected = changed_paths.iter().cloned().collect::<BTreeSet<_>>();
    let mut seen_paths = BTreeSet::new();
    let mut seen_groups = BTreeSet::new();
    let mut groups = Vec::with_capacity(output.commits.len());
    for commit in output.commits {
        validate_agent_group_label(&commit.group)?;
        if !seen_groups.insert(commit.group.clone()) {
            return Err(AppError::operation(format!(
                "semantic agent repeated group label `{}`",
                commit.group
            )));
        }
        validate_agent_message(&commit.message)?;
        if commit.paths.is_empty() {
            return Err(AppError::operation(format!(
                "semantic agent group `{}` has no paths",
                commit.group
            )));
        }
        let mut paths = commit.paths;
        let original_path_count = paths.len();
        paths.sort();
        paths.dedup();
        if paths.len() != original_path_count {
            return Err(AppError::operation(format!(
                "semantic agent group `{}` repeats a path",
                commit.group
            )));
        }
        for path in &paths {
            if !expected.contains(path) {
                return Err(AppError::operation(format!(
                    "semantic agent proposed unchanged or unknown path `{path}`"
                )));
            }
            if !seen_paths.insert(path.clone()) {
                return Err(AppError::operation(format!(
                    "semantic agent proposed path `{path}` more than once"
                )));
            }
        }
        groups.push(PlannedSemanticGroup {
            group: commit.group,
            message: commit.message.trim().to_string(),
            paths,
        });
    }
    if seen_paths != expected {
        let missing = expected
            .difference(&seen_paths)
            .cloned()
            .collect::<Vec<_>>();
        return Err(AppError::operation(format!(
            "semantic agent omitted changed paths: {}",
            missing.join(", ")
        )));
    }
    Ok(groups)
}

fn validate_agent_group_label(group: &str) -> Result<(), AppError> {
    if group.trim().is_empty()
        || group.len() > MAX_SEMANTIC_AGENT_LABEL_BYTES
        || group.chars().any(char::is_control)
    {
        return Err(AppError::operation(
            "semantic agent group labels must be non-empty bounded single-line text",
        ));
    }
    Ok(())
}

fn validate_agent_message(message: &str) -> Result<(), AppError> {
    if message.trim().is_empty()
        || message.len() > MAX_SEMANTIC_AGENT_MESSAGE_BYTES
        || message
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
        || message.lines().any(|line| {
            line.trim_start()
                .to_ascii_lowercase()
                .starts_with("vulcan-semantic-")
        })
    {
        return Err(AppError::operation(
            "semantic agent commit messages must be bounded text without reserved Vulcan trailers",
        ));
    }
    Ok(())
}

#[cfg(feature = "web")]
fn semantic_cancellation_check(cancellation: &SyncCancellationToken) -> Result<(), AppError> {
    if cancellation.is_cancelled() {
        Err(AppError::operation("semantic agent planning was cancelled"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_agent_output, SemanticAgentCommit, SemanticAgentOutput};

    #[cfg(feature = "web")]
    #[test]
    fn semantic_provider_rejects_credentials_over_remote_http() {
        let Err(error) = super::OpenAiCompatibleSemanticProvider::new(
            "http://api.example.com/v1",
            "fixture-model",
            Some("secret".to_string()),
        ) else {
            panic!("remote cleartext endpoint must fail");
        };
        assert!(error.to_string().contains("credentials require HTTPS"));
    }

    #[test]
    fn semantic_agent_output_requires_exact_once_only_path_coverage() {
        let changed = vec!["A.md".to_string(), "B.md".to_string()];
        let valid = validate_agent_output(
            &changed,
            SemanticAgentOutput {
                commits: vec![
                    SemanticAgentCommit {
                        group: "foundation".to_string(),
                        message: "Add the foundation".to_string(),
                        paths: vec!["B.md".to_string()],
                    },
                    SemanticAgentCommit {
                        group: "entrypoint".to_string(),
                        message: "Connect the entrypoint".to_string(),
                        paths: vec!["A.md".to_string()],
                    },
                ],
            },
        )
        .expect("valid agent grouping");
        assert_eq!(valid[0].group, "foundation");
        assert_eq!(valid[1].group, "entrypoint");

        for output in [
            SemanticAgentOutput {
                commits: vec![SemanticAgentCommit {
                    group: "partial".to_string(),
                    message: "Only one path".to_string(),
                    paths: vec!["A.md".to_string()],
                }],
            },
            SemanticAgentOutput {
                commits: vec![SemanticAgentCommit {
                    group: "unknown".to_string(),
                    message: "Invent a path".to_string(),
                    paths: vec!["A.md".to_string(), "C.md".to_string()],
                }],
            },
        ] {
            assert!(validate_agent_output(&changed, output).is_err());
        }
    }

    #[test]
    fn semantic_agent_messages_cannot_spoof_provenance_trailers() {
        let output = SemanticAgentOutput {
            commits: vec![SemanticAgentCommit {
                group: "notes".to_string(),
                message: "Update notes\n\nVulcan-Semantic-Target: forged".to_string(),
                paths: vec!["A.md".to_string()],
            }],
        };
        assert!(validate_agent_output(&["A.md".to_string()], output).is_err());
    }

    #[cfg(feature = "web")]
    #[test]
    fn a_cancelled_semantic_request_returns_without_waiting_for_the_provider() {
        use super::{
            OpenAiCompatibleSemanticProvider, SemanticAgentProvider, SemanticAgentRequest,
        };
        use crate::sync::SyncCancellationToken;
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        // A provider that accepts the request and never answers.
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("listener address");
        let _held = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("request");
            std::thread::sleep(Duration::from_secs(30));
            drop(stream);
        });
        let provider =
            OpenAiCompatibleSemanticProvider::new(&format!("http://{address}/v1"), "silent", None)
                .expect("provider");
        let cancellation = SyncCancellationToken::default();
        let canceller = cancellation.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            canceller.cancel();
        });

        let started = Instant::now();
        let error = provider
            .propose(
                &SemanticAgentRequest {
                    source_revision: "a".to_string(),
                    target_revision: "b".to_string(),
                    changes: vec![],
                },
                &cancellation,
            )
            .expect_err("a cancelled request fails");
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(feature = "web")]
    #[test]
    fn openai_semantic_provider_sends_bounded_patches_and_parses_exact_groups() {
        use super::{
            OpenAiCompatibleSemanticProvider, SemanticAgentChange, SemanticAgentProvider,
            SemanticAgentRequest,
        };
        use crate::sync::SyncCancellationToken;
        use std::io::{BufRead, BufReader, Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("request");
            let mut reader = BufReader::new(stream);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("header");
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            assert!(headers
                .to_ascii_lowercase()
                .contains("authorization: bearer secret"));
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_string)
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .expect("content length");
            let mut body = vec![0; length];
            reader.read_exact(&mut body).expect("request body");
            let body: serde_json::Value = serde_json::from_slice(&body).expect("request JSON");
            assert_eq!(body["model"], "fixture-model");
            assert!(body["messages"][1]["content"]
                .as_str()
                .is_some_and(|content| content.contains("diff --git") && content.contains("A.md")));
            let content = serde_json::json!({
                "commits": [{
                    "group": "notes",
                    "message": "Update the notes",
                    "paths": ["A.md"]
                }]
            })
            .to_string();
            let response = serde_json::json!({
                "id": "response-id",
                "choices": [{"message": {"role": "assistant", "content": content}}]
            })
            .to_string();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .expect("response");
        });
        let provider = OpenAiCompatibleSemanticProvider::new(
            &format!("http://{address}/v1"),
            "fixture-model",
            Some("secret".to_string()),
        )
        .expect("provider");
        let output = provider
            .propose(
                &SemanticAgentRequest {
                    source_revision: "0".repeat(40),
                    target_revision: "1".repeat(40),
                    changes: vec![SemanticAgentChange {
                        path: "A.md".to_string(),
                        patch: "diff --git a/A.md b/A.md".to_string(),
                    }],
                },
                &SyncCancellationToken::default(),
            )
            .expect("provider output");
        assert_eq!(output.commits[0].group, "notes");
        assert_eq!(output.commits[0].paths, ["A.md"]);
        server.join().expect("server thread");
    }
}
