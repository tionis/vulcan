//! Forgejo/Gitea repository deploy-key API.

use super::{DeployKey, ForgeConfig, ForgeDeployKeyAdapter};
use crate::AppError;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde_json::json;
use std::io::Read;
use std::time::Duration;

const PAGE_SIZE: usize = 50;
const MAX_PAGES: usize = 100;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_ERROR_CHARS: usize = 300;

pub struct ForgejoDeployKeys {
    client: Client,
    keys_url: Url,
    token: String,
}

#[derive(Deserialize)]
struct ApiKey {
    id: u64,
    key: String,
    title: String,
    #[serde(default)]
    read_only: bool,
}

impl From<ApiKey> for DeployKey {
    fn from(key: ApiKey) -> Self {
        Self {
            id: key.id,
            title: key.title,
            key: key.key,
            read_only: key.read_only,
        }
    }
}

impl ForgejoDeployKeys {
    pub fn new(config: &ForgeConfig, token: &str, timeout: Duration) -> Result<Self, AppError> {
        if token.trim().is_empty() {
            return Err(AppError::operation("forge API token is empty"));
        }
        let (owner, repo) = config.owner_and_repo()?;
        let mut base = Url::parse(&config.url)
            .map_err(|_| AppError::operation("forge URL must be a valid http(s) URL"))?;
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let keys_url = base
            .join(&format!("api/v1/repos/{owner}/{repo}/keys"))
            .map_err(|_| AppError::operation("failed to construct the forge API endpoint"))?;
        // Never follow redirects: the token must go only to the configured host.
        let client = Client::builder()
            .timeout(timeout)
            .redirect(Policy::none())
            .build()
            .map_err(|error| {
                AppError::operation(format!("failed to configure forge client: {error}"))
            })?;
        Ok(Self {
            client,
            keys_url,
            token: token.trim().to_owned(),
        })
    }

    fn request(&self, method: reqwest::Method, url: Url) -> reqwest::blocking::RequestBuilder {
        self.client
            .request(method, url)
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
    }

    fn read_body(response: reqwest::blocking::Response) -> Result<Vec<u8>, AppError> {
        let mut body = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(AppError::operation)?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(AppError::operation(
                "forge response exceeded its size limit",
            ));
        }
        Ok(body)
    }

    /// A short, single-line error built from the forge's own message.
    fn failure(status: StatusCode, body: &[u8], action: &str) -> AppError {
        let message = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
            .unwrap_or_default();
        let message = message
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_ERROR_CHARS)
            .collect::<String>();
        let hint = match status {
            StatusCode::UNAUTHORIZED => " (check the API token)",
            StatusCode::FORBIDDEN => " (the token needs write access to the repository)",
            StatusCode::NOT_FOUND => " (check the URL and repository, and the token's access)",
            _ => "",
        };
        if message.is_empty() {
            AppError::operation(format!("forge failed to {action}: HTTP {status}{hint}"))
        } else {
            AppError::operation(format!(
                "forge failed to {action}: HTTP {status}: {message}{hint}"
            ))
        }
    }
}

impl ForgeDeployKeyAdapter for ForgejoDeployKeys {
    fn list_deploy_keys(&self) -> Result<Vec<DeployKey>, AppError> {
        let mut keys = Vec::new();
        for page in 1..=MAX_PAGES {
            let mut url = self.keys_url.clone();
            url.query_pairs_mut()
                .append_pair("page", &page.to_string())
                .append_pair("limit", &PAGE_SIZE.to_string());
            let response = self
                .request(reqwest::Method::GET, url)
                .send()
                .map_err(|error| AppError::operation(format!("forge request failed: {error}")))?;
            let status = response.status();
            let body = Self::read_body(response)?;
            if !status.is_success() {
                return Err(Self::failure(status, &body, "list deploy keys"));
            }
            let batch: Vec<ApiKey> = serde_json::from_slice(&body)
                .map_err(|_| AppError::operation("forge returned an unexpected deploy-key list"))?;
            let count = batch.len();
            keys.extend(batch.into_iter().map(DeployKey::from));
            if count < PAGE_SIZE {
                return Ok(keys);
            }
        }
        Err(AppError::operation(
            "forge has more deploy keys than Vulcan will list; refusing to act on a partial list",
        ))
    }

    fn add_deploy_key(&self, public_key: &str, title: &str) -> Result<DeployKey, AppError> {
        let response = self
            .request(reqwest::Method::POST, self.keys_url.clone())
            .json(&json!({ "key": public_key, "title": title, "read_only": false }))
            .send()
            .map_err(|error| AppError::operation(format!("forge request failed: {error}")))?;
        let status = response.status();
        let body = Self::read_body(response)?;
        if status != StatusCode::CREATED && status != StatusCode::OK {
            return Err(Self::failure(status, &body, "add the deploy key"));
        }
        serde_json::from_slice::<ApiKey>(&body)
            .map(DeployKey::from)
            .map_err(|_| AppError::operation("forge returned an unexpected deploy-key response"))
    }

    fn remove_deploy_key(&self, id: u64) -> Result<(), AppError> {
        let url = Url::parse(&format!("{}/{id}", self.keys_url))
            .map_err(|_| AppError::operation("failed to construct the forge API endpoint"))?;
        let response = self
            .request(reqwest::Method::DELETE, url)
            .send()
            .map_err(|error| AppError::operation(format!("forge request failed: {error}")))?;
        let status = response.status();
        let body = Self::read_body(response)?;
        // A key that is already gone is the desired state.
        if status.is_success() || status == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Self::failure(status, &body, "remove the deploy key"))
        }
    }
}
