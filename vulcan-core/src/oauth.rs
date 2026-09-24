use base64::prelude::{Engine, BASE64_URL_SAFE_NO_PAD};
use jsonwebtoken::{
    decode, decode_header, encode, jwk::JwkSet, Algorithm, DecodingKey, EncodingKey, Header,
    Validation,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::io::Read;
use std::net::ToSocketAddrs;
use std::time::Duration;

const INDIEAUTH_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const INDIEAUTH_TOKEN_TIMEOUT: Duration = Duration::from_secs(10);
const INDIEAUTH_PROFILE_MAX_BYTES: u64 = 256 * 1024;
const INDIEAUTH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone)]
pub struct OAuthResourceServerConfig {
    pub issuer: String,
    pub audiences: Vec<String>,
    pub jwks_url: Option<String>,
    pub allowed_subs: Vec<String>,
    pub allowed_emails: Vec<String>,
    pub public_url: String,
}

#[derive(Debug, Clone)]
pub struct LocalOAuthIssuerConfig {
    pub public_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub signing_key: String,
    pub approval_token: String,
    pub subject: String,
    pub email: Option<String>,
    pub users: Vec<LocalOAuthUserConfig>,
    pub dcr_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOAuthUserConfig {
    pub subject: String,
    pub email: Option<String>,
    pub permission_profile: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientIdMetadataDocument {
    pub client_id: String,
    pub redirect_uris: Vec<String>,
    #[serde(default = "default_public_client_auth_method")]
    pub token_endpoint_auth_method: String,
}

fn default_public_client_auth_method() -> String {
    "none".to_string()
}

pub fn fetch_client_id_metadata(client_id: &str) -> Result<ClientIdMetadataDocument, OAuthError> {
    const MAX_METADATA_BYTES: u64 = 64 * 1024;
    let url = reqwest::Url::parse(client_id)
        .map_err(|error| OAuthError::Config(format!("invalid client ID URL: {error}")))?;
    let lowercase_host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || lowercase_host == "localhost"
        || lowercase_host.ends_with(".localhost")
        || lowercase_host
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix == "local")
        || url
            .host_str()
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(ip_is_non_public)
    {
        return Err(OAuthError::Config(
            "Client ID Metadata Document URL must be public HTTPS without credentials, query, or fragment"
                .to_string(),
        ));
    }
    let host = url.host_str().expect("validated URL has a host");
    let pinned_address = if host.parse::<std::net::IpAddr>().is_ok() {
        None
    } else {
        let addresses = (host, url.port_or_known_default().unwrap_or(443))
            .to_socket_addrs()
            .map_err(|error| OAuthError::Network(error.to_string()))?
            .collect::<Vec<_>>();
        if addresses.is_empty()
            || addresses
                .iter()
                .any(|address| ip_is_non_public(address.ip()))
        {
            return Err(OAuthError::Config(
                "Client ID Metadata Document host did not resolve exclusively to public addresses"
                    .to_string(),
            ));
        }
        addresses.into_iter().next()
    };
    let mut client_builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none());
    if let Some(address) = pinned_address {
        client_builder = client_builder.resolve(host, address);
    }
    let response = client_builder
        .build()
        .map_err(|error| OAuthError::Network(error.to_string()))?
        .get(url)
        .send()
        .map_err(|error| OAuthError::Network(error.to_string()))?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|length| length > MAX_METADATA_BYTES)
    {
        return Err(OAuthError::Network(
            "Client ID Metadata Document request failed or was too large".to_string(),
        ));
    }
    let bytes = response
        .bytes()
        .map_err(|error| OAuthError::Network(error.to_string()))?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(OAuthError::Network(
            "Client ID Metadata Document exceeded 64 KiB".to_string(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| OAuthError::Network(format!("invalid client metadata JSON: {error}")))
}

fn ip_is_non_public(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => {
            let octets = address.octets();
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_broadcast()
                || address.is_documentation()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        }
        std::net::IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_unique_local()
                || address.is_unicast_link_local()
        }
    }
}

#[derive(Debug, Clone)]
pub struct OAuthResourceServer {
    issuer: String,
    audiences: Vec<String>,
    allowed_subs: BTreeSet<String>,
    allowed_emails: BTreeSet<String>,
    public_url: String,
    authorization_server_issuer: String,
    authorization_server_metadata: Value,
    protected_resource_metadata_url: String,
    jwks: JwkSet,
}

#[derive(Debug, Clone)]
pub struct LocalOAuthIssuer {
    public_url: String,
    client_id: String,
    client_secret: String,
    signing_key: String,
    approval_token: String,
    subject: String,
    email: Option<String>,
    users: Vec<LocalOAuthUserConfig>,
    protected_resource_metadata_url: String,
    authorization_server_metadata: Value,
}

impl OAuthResourceServer {
    pub fn from_config(config: OAuthResourceServerConfig) -> Result<Self, OAuthError> {
        if !config.public_url.starts_with("https://") {
            return Err(OAuthError::Config(
                "public OAuth resource URL must use HTTPS".to_string(),
            ));
        }
        if config.audiences.is_empty() {
            return Err(OAuthError::Config(
                "at least one OAuth audience is required".to_string(),
            ));
        }
        if config.allowed_subs.is_empty() && config.allowed_emails.is_empty() {
            return Err(OAuthError::Config(
                "at least one allowed OAuth subject or email is required".to_string(),
            ));
        }
        let (discovery, discovery_value) = discover_oidc_metadata(&config.issuer)?;
        let issuer = discovery.issuer;
        let jwks_url = match config.jwks_url.as_deref() {
            Some(url) => url.to_string(),
            None => discovery.jwks_uri,
        };
        let jwks = fetch_jwks(&jwks_url)?;
        let protected_resource_metadata_url = protected_resource_metadata_url(&config.public_url)?;
        let authorization_server_issuer = config.public_url.clone();
        let authorization_server_metadata =
            authorization_server_metadata(&authorization_server_issuer, discovery_value)?;
        Ok(Self {
            issuer,
            audiences: config.audiences,
            allowed_subs: config.allowed_subs.into_iter().collect(),
            allowed_emails: config.allowed_emails.into_iter().collect(),
            public_url: config.public_url,
            authorization_server_issuer,
            authorization_server_metadata,
            protected_resource_metadata_url,
            jwks,
        })
    }

    pub fn validate_bearer_token(&self, token: &str) -> Result<OAuthTokenIdentity, OAuthError> {
        let header = decode_header(token)
            .map_err(|error| OAuthError::Token(format!("invalid JWT header: {error}")))?;
        let algorithm = header.alg;
        if !oauth_algorithm_allowed(algorithm) {
            return Err(OAuthError::Token(format!(
                "unsupported OAuth JWT algorithm: {algorithm:?}"
            )));
        }
        let kid = header
            .kid
            .as_deref()
            .ok_or_else(|| OAuthError::Token("OAuth JWT is missing a key id".to_string()))?;
        let jwk = self.jwks.find(kid).ok_or_else(|| {
            OAuthError::Token("OAuth JWT key id is not present in JWKS".to_string())
        })?;
        let decoding_key = DecodingKey::from_jwk(jwk)
            .map_err(|error| OAuthError::Token(format!("invalid OAuth JWKS key: {error}")))?;
        let mut validation = Validation::new(algorithm);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.set_audience(
            &self
                .audiences
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        validation.leeway = 60;
        let token = decode::<OAuthClaims>(token, &decoding_key, &validation)
            .map_err(|error| OAuthError::Token(format!("invalid OAuth bearer token: {error}")))?;
        let claims = token.claims;
        if self.allowed_subs.contains(&claims.sub)
            || claims
                .email
                .as_deref()
                .is_some_and(|email| self.allowed_emails.contains(email))
        {
            return Ok(OAuthTokenIdentity {
                subject: claims.sub,
                email: claims.email,
                scopes: claims.scope.into_scopes(),
            });
        }
        Err(OAuthError::Token(
            "OAuth token subject is not allowed".to_string(),
        ))
    }

    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    #[must_use]
    pub fn public_url(&self) -> &str {
        &self.public_url
    }

    #[must_use]
    pub fn authorization_server_issuer(&self) -> &str {
        &self.authorization_server_issuer
    }

    #[must_use]
    pub fn authorization_server_metadata(&self) -> &Value {
        &self.authorization_server_metadata
    }

    #[must_use]
    pub fn protected_resource_metadata_url(&self) -> &str {
        &self.protected_resource_metadata_url
    }
}

impl LocalOAuthIssuer {
    pub fn from_config(config: LocalOAuthIssuerConfig) -> Result<Self, OAuthError> {
        if !config.public_url.starts_with("https://") {
            return Err(OAuthError::Config(
                "public OAuth resource URL must use HTTPS".to_string(),
            ));
        }
        if config.client_id.is_empty()
            || config.client_secret.is_empty()
            || config.signing_key.is_empty()
            || config.subject.is_empty()
        {
            return Err(OAuthError::Config(
                "local OAuth issuer requires non-empty client id, client secret, signing key, and subject"
                    .to_string(),
            ));
        }
        if config.client_secret == config.signing_key {
            return Err(OAuthError::Config(
                "local OAuth token signing key must be distinct from the client secret".to_string(),
            ));
        }
        let protected_resource_metadata_url = protected_resource_metadata_url(&config.public_url)?;
        let origin = public_url_origin(&config.public_url)?;
        let mut authorization_server_metadata = serde_json::json!({
            "issuer": config.public_url,
            "authorization_endpoint": format!("{origin}/oauth/authorize"),
            "token_endpoint": format!("{origin}/oauth/token"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
            "code_challenge_methods_supported": ["S256"],
            "scopes_supported": ["openid", "email", "profile", "mcp:tools", "mcp:resources", "mcp:prompts"],
        });
        if config.dcr_enabled {
            authorization_server_metadata["registration_endpoint"] =
                Value::String(format!("{origin}/oauth/register"));
        }
        Ok(Self {
            public_url: config.public_url,
            client_id: config.client_id,
            client_secret: config.client_secret,
            signing_key: config.signing_key,
            approval_token: config.approval_token,
            subject: config.subject,
            email: config.email,
            users: config.users,
            protected_resource_metadata_url,
            authorization_server_metadata,
        })
    }

    pub fn validate_bearer_token(
        &self,
        token: &str,
    ) -> Result<LocalOAuthTokenIdentity, OAuthError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[self.public_url.as_str()]);
        validation.set_audience(&[self.public_url.as_str()]);
        validation.leeway = 60;
        let token = decode::<LocalOAuthClaims>(
            token,
            &DecodingKey::from_secret(self.signing_key.as_bytes()),
            &validation,
        )
        .map_err(|error| OAuthError::Token(format!("invalid local OAuth bearer token: {error}")))?;
        if let Some(user) = self.user_for_subject(&token.claims.sub) {
            Ok(LocalOAuthTokenIdentity {
                subject: token.claims.sub,
                email: user.email,
                permission_profile: user.permission_profile,
                client_id: token.claims.client_id,
                scopes: token.claims.scope,
                grant_id: token.claims.grant_id,
            })
        } else {
            Err(OAuthError::Token(
                "local OAuth token subject is not allowed".to_string(),
            ))
        }
    }

    pub fn issue_access_token(&self) -> Result<String, OAuthError> {
        self.issue_access_token_for(&self.subject)
    }

    pub fn issue_access_token_for(&self, subject: &str) -> Result<String, OAuthError> {
        self.issue_access_token_for_authorization(
            subject,
            &self.client_id,
            &[
                "openid".to_string(),
                "email".to_string(),
                "profile".to_string(),
            ],
            None,
        )
    }

    pub fn issue_access_token_for_authorization(
        &self,
        subject: &str,
        client_id: &str,
        scopes: &[String],
        grant_id: Option<String>,
    ) -> Result<String, OAuthError> {
        let Some(user) = self.user_for_subject(subject) else {
            return Err(OAuthError::Token(
                "local OAuth token subject is not allowed".to_string(),
            ));
        };
        let now = unix_timestamp();
        let claims = LocalOAuthClaims {
            iss: self.public_url.clone(),
            sub: subject.to_string(),
            aud: vec![self.public_url.clone()],
            exp: now + 900,
            iat: now,
            email: user.email,
            permission_profile: user.permission_profile,
            client_id: Some(client_id.to_string()),
            scope: scopes.to_vec(),
            grant_id,
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.signing_key.as_bytes()),
        )
        .map_err(|error| OAuthError::Token(format!("failed to issue OAuth token: {error}")))
    }

    #[must_use]
    pub fn verify_client(&self, client_id: &str, client_secret: &str) -> bool {
        client_id == self.client_id && client_secret == self.client_secret
    }

    #[must_use]
    pub fn verify_approval_token(&self, approval_token: &str) -> bool {
        approval_token == self.approval_token
    }

    #[must_use]
    pub fn user_for_subject(&self, subject: &str) -> Option<LocalOAuthUserConfig> {
        self.users
            .iter()
            .find(|user| subjects_match(&user.subject, subject))
            .cloned()
            .or_else(|| {
                subjects_match(&self.subject, subject).then(|| LocalOAuthUserConfig {
                    subject: self.subject.clone(),
                    email: self.email.clone(),
                    permission_profile: None,
                })
            })
    }

    #[must_use]
    pub fn default_user(&self) -> LocalOAuthUserConfig {
        LocalOAuthUserConfig {
            subject: self.subject.clone(),
            email: self.email.clone(),
            permission_profile: None,
        }
    }

    #[must_use]
    pub fn verify_pkce_s256(&self, verifier: &str, challenge: &str) -> bool {
        let digest = Sha256::digest(verifier.as_bytes());
        BASE64_URL_SAFE_NO_PAD.encode(digest) == challenge
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    #[must_use]
    pub fn public_url(&self) -> &str {
        &self.public_url
    }

    #[must_use]
    pub fn authorization_server_metadata(&self) -> &Value {
        &self.authorization_server_metadata
    }

    #[must_use]
    pub fn protected_resource_metadata_url(&self) -> &str {
        &self.protected_resource_metadata_url
    }
}

#[derive(Debug)]
pub enum OAuthError {
    Config(String),
    Network(String),
    Token(String),
}

impl fmt::Display for OAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) | Self::Network(message) | Self::Token(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl Error for OAuthError {}

#[derive(Debug, Deserialize)]
struct OidcDiscoveryDocument {
    issuer: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct OAuthClaims {
    sub: String,
    email: Option<String>,
    #[serde(default)]
    scope: OAuthScopeClaim,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum OAuthScopeClaim {
    #[default]
    Missing,
    Text(String),
    List(Vec<String>),
}

impl OAuthScopeClaim {
    fn into_scopes(self) -> Vec<String> {
        match self {
            Self::Missing => Vec::new(),
            Self::Text(value) => value
                .split_ascii_whitespace()
                .map(ToOwned::to_owned)
                .collect(),
            Self::List(values) => values,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct LocalOAuthClaims {
    iss: String,
    sub: String,
    aud: Vec<String>,
    exp: u64,
    iat: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permission_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scope: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grant_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOAuthTokenIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub permission_profile: Option<String>,
    pub client_id: Option<String>,
    pub scopes: Vec<String>,
    pub grant_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthTokenIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndieAuthEndpoints {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
}

pub fn discover_indieauth_endpoints(me: &str) -> Result<IndieAuthEndpoints, OAuthError> {
    let profile_url = parse_https_url(me, "IndieAuth profile URL")?;
    let response = indieauth_http_client(INDIEAUTH_DISCOVERY_TIMEOUT)?
        .get(profile_url)
        .header("Accept", "text/html, application/xhtml+xml, */*")
        .send()
        .map_err(|error| OAuthError::Network(format!("IndieAuth profile fetch failed: {error}")))?
        .error_for_status()
        .map_err(|error| OAuthError::Network(format!("IndieAuth profile fetch failed: {error}")))?;
    reject_indieauth_redirect(&response, "profile")?;
    let final_url = response.url().clone();
    let headers = response.headers().clone();
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = read_bounded_indieauth_text(response, INDIEAUTH_PROFILE_MAX_BYTES, "profile")?;
    let base_url = final_url.as_str();
    if let Some(metadata_url) = discover_link_header_rel(&headers, "indieauth-metadata")
        .and_then(|url| resolve_url(base_url, &url))
        .or_else(|| {
            content_type
                .contains("html")
                .then(|| discover_html_link_rel(&body, "indieauth-metadata"))
                .flatten()
                .and_then(|url| resolve_url(base_url, &url))
        })
    {
        return fetch_indieauth_metadata(&metadata_url);
    }
    let authorization_endpoint = discover_link_header_rel(&headers, "authorization_endpoint")
        .or_else(|| {
            content_type
                .contains("html")
                .then(|| discover_html_link_rel(&body, "authorization_endpoint"))
                .flatten()
        })
        .and_then(|url| resolve_url(base_url, &url));
    let token_endpoint = discover_link_header_rel(&headers, "token_endpoint")
        .or_else(|| {
            content_type
                .contains("html")
                .then(|| discover_html_link_rel(&body, "token_endpoint"))
                .flatten()
        })
        .and_then(|url| resolve_url(base_url, &url));
    match (authorization_endpoint, token_endpoint) {
        (Some(authorization_endpoint), Some(token_endpoint)) => {
            ensure_https_url(&authorization_endpoint, "IndieAuth authorization endpoint")?;
            ensure_https_url(&token_endpoint, "IndieAuth token endpoint")?;
            Ok(IndieAuthEndpoints {
                authorization_endpoint,
                token_endpoint,
            })
        }
        _ => Err(OAuthError::Network(
            "IndieAuth discovery did not find authorization and token endpoints".to_string(),
        )),
    }
}

pub fn exchange_indieauth_code(
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    code_verifier: &str,
) -> Result<String, OAuthError> {
    let token_endpoint = parse_https_url(token_endpoint, "IndieAuth token endpoint")?;
    let response = indieauth_http_client(INDIEAUTH_TOKEN_TIMEOUT)?
        .post(token_endpoint)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ])
        .send()
        .map_err(|error| OAuthError::Network(format!("IndieAuth token request failed: {error}")))?
        .error_for_status()
        .map_err(|error| OAuthError::Network(format!("IndieAuth token request failed: {error}")))?;
    reject_indieauth_redirect(&response, "token")?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = read_bounded_indieauth_text(response, INDIEAUTH_RESPONSE_MAX_BYTES, "token")?;
    if content_type.contains("application/json") {
        let value = serde_json::from_str::<Value>(&body).map_err(|error| {
            OAuthError::Network(format!("invalid IndieAuth token JSON: {error}"))
        })?;
        return value
            .get("me")
            .or_else(|| value.get("sub"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OAuthError::Token("IndieAuth token response did not include me or sub".to_string())
            });
    }
    parse_form_body(&body)
        .remove("me")
        .or_else(|| parse_form_body(&body).remove("sub"))
        .ok_or_else(|| {
            OAuthError::Token("IndieAuth token response did not include me or sub".to_string())
        })
}

#[must_use]
pub fn pkce_s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    BASE64_URL_SAFE_NO_PAD.encode(digest)
}

fn subjects_match(configured: &str, actual: &str) -> bool {
    configured == actual
        || normalized_url_subject(configured)
            .zip(normalized_url_subject(actual))
            .is_some_and(|(configured, actual)| configured == actual)
}

fn normalized_url_subject(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let mut normalized = format!("{}://{}", url.scheme(), host);
    if let Some(port) = url.port() {
        normalized.push(':');
        normalized.push_str(&port.to_string());
    }
    let path = url.path().trim_end_matches('/');
    if !path.is_empty() {
        normalized.push_str(path);
    }
    if let Some(query) = url.query() {
        normalized.push('?');
        normalized.push_str(query);
    }
    Some(normalized)
}

#[derive(Debug, Deserialize)]
struct IndieAuthMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
}

fn fetch_indieauth_metadata(metadata_url: &str) -> Result<IndieAuthEndpoints, OAuthError> {
    let metadata_url = parse_https_url(metadata_url, "IndieAuth metadata endpoint")?;
    let response = indieauth_http_client(INDIEAUTH_DISCOVERY_TIMEOUT)?
        .get(metadata_url)
        .send()
        .map_err(|error| OAuthError::Network(format!("IndieAuth metadata fetch failed: {error}")))?
        .error_for_status()
        .map_err(|error| {
            OAuthError::Network(format!("IndieAuth metadata fetch failed: {error}"))
        })?;
    reject_indieauth_redirect(&response, "metadata")?;
    let body = read_bounded_indieauth_text(response, INDIEAUTH_RESPONSE_MAX_BYTES, "metadata")?;
    let metadata = serde_json::from_str::<IndieAuthMetadata>(&body).map_err(|error| {
        OAuthError::Network(format!("invalid IndieAuth metadata JSON: {error}"))
    })?;
    ensure_https_url(
        &metadata.authorization_endpoint,
        "IndieAuth authorization endpoint",
    )?;
    ensure_https_url(&metadata.token_endpoint, "IndieAuth token endpoint")?;
    Ok(IndieAuthEndpoints {
        authorization_endpoint: metadata.authorization_endpoint,
        token_endpoint: metadata.token_endpoint,
    })
}

fn indieauth_http_client(timeout: Duration) -> Result<reqwest::blocking::Client, OAuthError> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| OAuthError::Network(format!("IndieAuth HTTP client failed: {error}")))
}

fn reject_indieauth_redirect(
    response: &reqwest::blocking::Response,
    stage: &str,
) -> Result<(), OAuthError> {
    if response.status().is_redirection() {
        return Err(OAuthError::Network(format!(
            "IndieAuth {stage} redirected; configure the final canonical HTTPS URL"
        )));
    }
    Ok(())
}

fn read_bounded_indieauth_text(
    response: impl Read,
    maximum_bytes: u64,
    stage: &str,
) -> Result<String, OAuthError> {
    let mut bytes = Vec::new();
    response
        .take(maximum_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            OAuthError::Network(format!("IndieAuth {stage} response read failed: {error}"))
        })?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(OAuthError::Network(format!(
            "IndieAuth {stage} response exceeds {maximum_bytes} bytes"
        )));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_https_url(raw: &str, label: &str) -> Result<reqwest::Url, OAuthError> {
    let url = reqwest::Url::parse(raw).map_err(|error| {
        OAuthError::Network(format!("{label} must be an absolute URL: {error}"))
    })?;
    if url.scheme() != "https" {
        return Err(OAuthError::Network(format!("{label} must use HTTPS")));
    }
    Ok(url)
}

fn ensure_https_url(raw: &str, label: &str) -> Result<(), OAuthError> {
    parse_https_url(raw, label).map(|_| ())
}

fn discover_link_header_rel(
    headers: &reqwest::header::HeaderMap,
    target_rel: &str,
) -> Option<String> {
    headers
        .get_all(reqwest::header::LINK)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| discover_link_header_value_rel(value, target_rel))
}

fn discover_link_header_value_rel(value: &str, target_rel: &str) -> Option<String> {
    value.split(',').find_map(|part| {
        let part = part.trim();
        let (url_part, params) = part.split_once('>')?;
        let url = url_part.trim().strip_prefix('<')?.trim();
        params
            .split(';')
            .filter_map(|param| param.trim().split_once('='))
            .any(|(name, value)| {
                name.eq_ignore_ascii_case("rel")
                    && value
                        .trim_matches('"')
                        .split_whitespace()
                        .any(|rel| rel == target_rel)
            })
            .then(|| url.to_string())
    })
}

fn discover_html_link_rel(html: &str, target_rel: &str) -> Option<String> {
    let link_re = Regex::new(r"(?is)<link\b[^>]*>").expect("link regex should compile");
    let result = link_re.find_iter(html).find_map(|link| {
        let tag = link.as_str();
        let rel = html_attribute(tag, "rel")?;
        rel.split_whitespace()
            .any(|candidate| candidate == target_rel)
            .then(|| html_attribute(tag, "href"))
            .flatten()
    });
    result
}

fn html_attribute(tag: &str, name: &str) -> Option<String> {
    let pattern = format!(r#"(?is)\b{name}\s*=\s*("([^"]*)"|'([^']*)'|([^\s>]+))"#);
    let re = Regex::new(&pattern).expect("attribute regex should compile");
    let captures = re.captures(tag)?;
    captures
        .get(2)
        .or_else(|| captures.get(3))
        .or_else(|| captures.get(4))
        .map(|value| value.as_str().to_string())
}

fn resolve_url(base_url: &str, url: &str) -> Option<String> {
    reqwest::Url::parse(base_url)
        .ok()?
        .join(url)
        .ok()
        .map(Into::into)
}

fn parse_form_body(body: &str) -> std::collections::BTreeMap<String, String> {
    body.split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            Some((percent_decode_form(key)?, percent_decode_form(value)?))
        })
        .collect()
}

fn percent_decode_form(value: &str) -> Option<String> {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                output.push(high * 16 + low);
                index += 3;
            }
            b'%' => return None,
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn discover_oidc_metadata(issuer: &str) -> Result<(OidcDiscoveryDocument, Value), OAuthError> {
    let discovery_url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let value = reqwest::blocking::get(&discovery_url)
        .map_err(|error| OAuthError::Network(format!("failed to fetch OIDC discovery: {error}")))?
        .error_for_status()
        .map_err(|error| OAuthError::Network(format!("OIDC discovery failed: {error}")))?
        .json::<Value>()
        .map_err(|error| OAuthError::Network(format!("invalid OIDC discovery JSON: {error}")))?;
    let document = serde_json::from_value::<OidcDiscoveryDocument>(value.clone())
        .map_err(|error| OAuthError::Network(format!("invalid OIDC discovery JSON: {error}")))?;
    Ok((document, value))
}

fn fetch_jwks(jwks_url: &str) -> Result<JwkSet, OAuthError> {
    reqwest::blocking::get(jwks_url)
        .map_err(|error| OAuthError::Network(format!("failed to fetch OAuth JWKS: {error}")))?
        .error_for_status()
        .map_err(|error| OAuthError::Network(format!("OAuth JWKS fetch failed: {error}")))?
        .json::<JwkSet>()
        .map_err(|error| OAuthError::Network(format!("invalid OAuth JWKS JSON: {error}")))
}

pub fn protected_resource_metadata_url(public_url: &str) -> Result<String, OAuthError> {
    let Some((scheme, rest)) = public_url.split_once("://") else {
        return Err(OAuthError::Config(
            "public OAuth resource URL must be absolute".to_string(),
        ));
    };
    let Some((host, path)) = rest.split_once('/') else {
        return Ok(format!(
            "{scheme}://{rest}/.well-known/oauth-protected-resource"
        ));
    };
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        Ok(format!(
            "{scheme}://{host}/.well-known/oauth-protected-resource"
        ))
    } else {
        Ok(format!(
            "{scheme}://{host}/.well-known/oauth-protected-resource/{path}"
        ))
    }
}

fn public_url_origin(public_url: &str) -> Result<String, OAuthError> {
    let Some((scheme, rest)) = public_url.split_once("://") else {
        return Err(OAuthError::Config(
            "public OAuth resource URL must be absolute".to_string(),
        ));
    };
    let host = rest.split('/').next().unwrap_or(rest);
    Ok(format!("{scheme}://{host}"))
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn authorization_server_metadata(issuer: &str, discovery: Value) -> Result<Value, OAuthError> {
    let Value::Object(mut metadata) = discovery else {
        return Err(OAuthError::Network(
            "OIDC discovery document must be a JSON object".to_string(),
        ));
    };
    metadata.insert("issuer".to_string(), Value::String(issuer.to_string()));
    metadata
        .entry("response_types_supported")
        .or_insert_with(|| serde_json::json!(["code"]));
    metadata
        .entry("grant_types_supported")
        .or_insert_with(|| serde_json::json!(["authorization_code", "refresh_token"]));
    metadata
        .entry("code_challenge_methods_supported")
        .or_insert_with(|| serde_json::json!(["S256"]));
    Ok(Value::Object(metadata))
}

fn oauth_algorithm_allowed(algorithm: Algorithm) -> bool {
    matches!(
        algorithm,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::ES256
            | Algorithm::ES384
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indieauth_response_reader_rejects_oversized_profile_and_metadata() {
        let exact = vec![b'a'; 8];
        assert_eq!(
            read_bounded_indieauth_text(std::io::Cursor::new(&exact), 8, "profile")
                .expect("bounded response"),
            "aaaaaaaa"
        );
        let oversized = vec![b'a'; 9];
        let error = read_bounded_indieauth_text(std::io::Cursor::new(&oversized), 8, "metadata")
            .expect_err("oversized response");
        assert!(error
            .to_string()
            .contains("metadata response exceeds 8 bytes"));
    }

    #[test]
    fn indieauth_http_client_does_not_follow_redirects() {
        use std::io::Write;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("client");
            let mut method = [0; 3];
            stream.read_exact(&mut method).expect("request method");
            assert_eq!(&method, b"GET");
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: https://example.test/final\r\nContent-Length: 0\r\n\r\n")
                .expect("redirect");
        });
        let response = indieauth_http_client(Duration::from_secs(1))
            .expect("client")
            .get(format!("http://{address}/profile"))
            .send()
            .expect("response");
        let error = reject_indieauth_redirect(&response, "profile")
            .expect_err("redirect requires explicit canonical identity URL");
        assert!(error.to_string().contains("final canonical HTTPS URL"));
        server.join().expect("server");
    }

    #[test]
    fn indieauth_http_client_times_out_on_a_stalled_response() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("client");
            std::thread::sleep(Duration::from_millis(200));
        });
        let error = indieauth_http_client(Duration::from_millis(50))
            .expect("client")
            .get(format!("http://{address}/profile"))
            .send()
            .expect_err("stalled response must time out");
        assert!(error.is_timeout());
        server.join().expect("server");
    }

    #[test]
    fn external_oauth_rejects_unexpected_jwt_algorithms() {
        assert!(oauth_algorithm_allowed(Algorithm::RS256));
        assert!(oauth_algorithm_allowed(Algorithm::ES256));
        assert!(!oauth_algorithm_allowed(Algorithm::HS256));
        assert!(!oauth_algorithm_allowed(Algorithm::HS384));
        assert!(!oauth_algorithm_allowed(Algorithm::EdDSA));
    }

    #[test]
    fn protected_resource_metadata_url_tracks_endpoint_path() {
        assert_eq!(
            protected_resource_metadata_url("https://wiki.example.test/mcp").unwrap(),
            "https://wiki.example.test/.well-known/oauth-protected-resource/mcp"
        );
        assert_eq!(
            protected_resource_metadata_url("https://wiki.example.test").unwrap(),
            "https://wiki.example.test/.well-known/oauth-protected-resource"
        );
    }

    #[test]
    fn protected_resource_metadata_url_rejects_relative_urls() {
        assert!(protected_resource_metadata_url("/mcp").is_err());
    }

    #[test]
    fn authorization_server_metadata_uses_public_shim_issuer() {
        let metadata = authorization_server_metadata(
            "https://wiki.example.test/mcp",
            serde_json::json!({
                "issuer": "https://auth.example.test/application/o/vulcan-mcp/",
                "authorization_endpoint": "https://auth.example.test/application/o/authorize/",
                "token_endpoint": "https://auth.example.test/application/o/token/",
                "jwks_uri": "https://auth.example.test/application/o/vulcan-mcp/jwks/",
            }),
        )
        .unwrap();
        assert_eq!(metadata["issuer"], "https://wiki.example.test/mcp");
        assert_eq!(
            metadata["authorization_endpoint"],
            "https://auth.example.test/application/o/authorize/"
        );
        assert_eq!(
            metadata["response_types_supported"],
            serde_json::json!(["code"])
        );
        assert_eq!(
            metadata["code_challenge_methods_supported"],
            serde_json::json!(["S256"])
        );
    }

    #[test]
    fn pkce_s256_challenge_matches_rfc_vector() {
        assert_eq!(
            pkce_s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn local_oauth_issuer_issues_and_validates_tokens() {
        let issuer = LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://wiki.example.test/mcp".to_string(),
            client_id: "vulcan-mcp".to_string(),
            client_secret: "secret".to_string(),
            signing_key: "server-signing-key".to_string(),
            approval_token: "approve".to_string(),
            subject: "eric".to_string(),
            email: Some("eric@example.test".to_string()),
            users: Vec::new(),
            dcr_enabled: false,
        })
        .unwrap();
        assert!(issuer.verify_client("vulcan-mcp", "secret"));
        assert!(issuer.verify_approval_token("approve"));
        let token = issuer.issue_access_token().unwrap();
        let identity = issuer.validate_bearer_token(&token).unwrap();
        assert_eq!(identity.subject, "eric");
        assert_eq!(
            issuer.authorization_server_metadata()["issuer"],
            issuer.public_url()
        );
    }

    #[test]
    fn local_oauth_issuer_embeds_bound_permission_profile() {
        let issuer = LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://wiki.example.test/mcp".to_string(),
            client_id: "vulcan-mcp".to_string(),
            client_secret: "secret".to_string(),
            signing_key: "server-signing-key".to_string(),
            approval_token: String::new(),
            subject: "fallback".to_string(),
            email: None,
            users: vec![LocalOAuthUserConfig {
                subject: "https://tionis.dev/".to_string(),
                email: Some("eric@example.test".to_string()),
                permission_profile: Some("daily-wiki-agent".to_string()),
            }],
            dcr_enabled: true,
        })
        .unwrap();
        assert!(issuer.authorization_server_metadata()["registration_endpoint"].is_string());
        let user = issuer.user_for_subject("https://tionis.dev/").unwrap();
        let token = issuer.issue_access_token_for(&user.subject).unwrap();
        let identity = issuer.validate_bearer_token(&token).unwrap();
        assert_eq!(identity.subject, "https://tionis.dev/");
        assert_eq!(
            identity.permission_profile.as_deref(),
            Some("daily-wiki-agent")
        );
        assert_eq!(identity.client_id.as_deref(), Some("vulcan-mcp"));
        assert_eq!(identity.scopes, ["openid", "email", "profile"]);
        assert_eq!(identity.grant_id, None);

        let token = issuer
            .issue_access_token_for_authorization(
                &user.subject,
                "dynamic-client",
                &["mcp:resources".to_string(), "mcp:tools".to_string()],
                Some("01GRANT".to_string()),
            )
            .unwrap();
        let identity = issuer.validate_bearer_token(&token).unwrap();
        assert_eq!(identity.client_id.as_deref(), Some("dynamic-client"));
        assert_eq!(identity.scopes, ["mcp:resources", "mcp:tools"]);
        assert_eq!(identity.grant_id.as_deref(), Some("01GRANT"));
    }

    #[test]
    fn local_oauth_rejects_tokens_signed_with_the_client_secret() {
        let issuer = LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://wiki.example.test/mcp".to_string(),
            client_id: "vulcan-mcp".to_string(),
            client_secret: "client-secret".to_string(),
            signing_key: "server-signing-key".to_string(),
            approval_token: "approve".to_string(),
            subject: "eric".to_string(),
            email: None,
            users: Vec::new(),
            dcr_enabled: false,
        })
        .unwrap();
        let now = unix_timestamp();
        let forged = encode(
            &Header::new(Algorithm::HS256),
            &LocalOAuthClaims {
                iss: issuer.public_url.clone(),
                sub: "eric".to_string(),
                aud: vec![issuer.public_url.clone()],
                exp: now + 3600,
                iat: now,
                email: None,
                permission_profile: Some("admin".to_string()),
                client_id: None,
                scope: Vec::new(),
                grant_id: None,
            },
            &EncodingKey::from_secret(b"client-secret"),
        )
        .unwrap();

        assert!(issuer.validate_bearer_token(&forged).is_err());
    }

    #[test]
    fn local_oauth_uses_server_side_permission_profile_binding() {
        let issuer = LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://wiki.example.test/mcp".to_string(),
            client_id: "vulcan-mcp".to_string(),
            client_secret: "client-secret".to_string(),
            signing_key: "server-signing-key".to_string(),
            approval_token: "approve".to_string(),
            subject: "fallback".to_string(),
            email: None,
            users: vec![LocalOAuthUserConfig {
                subject: "eric".to_string(),
                email: Some("eric@example.test".to_string()),
                permission_profile: Some("readonly".to_string()),
            }],
            dcr_enabled: false,
        })
        .unwrap();
        let now = unix_timestamp();
        let forged_claim = encode(
            &Header::new(Algorithm::HS256),
            &LocalOAuthClaims {
                iss: issuer.public_url.clone(),
                sub: "eric".to_string(),
                aud: vec![issuer.public_url.clone()],
                exp: now + 3600,
                iat: now,
                email: Some("attacker@example.test".to_string()),
                permission_profile: Some("admin".to_string()),
                client_id: None,
                scope: Vec::new(),
                grant_id: None,
            },
            &EncodingKey::from_secret(b"server-signing-key"),
        )
        .unwrap();

        let identity = issuer.validate_bearer_token(&forged_claim).unwrap();
        assert_eq!(identity.email.as_deref(), Some("eric@example.test"));
        assert_eq!(identity.permission_profile.as_deref(), Some("readonly"));
    }

    #[test]
    fn local_oauth_issuer_matches_indieauth_url_subjects_canonically() {
        let issuer = LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://wiki.example.test/mcp".to_string(),
            client_id: "vulcan-mcp".to_string(),
            client_secret: "secret".to_string(),
            signing_key: "server-signing-key".to_string(),
            approval_token: String::new(),
            subject: "fallback".to_string(),
            email: None,
            users: vec![LocalOAuthUserConfig {
                subject: "https://eric.wendland.dev".to_string(),
                email: Some("eric@example.test".to_string()),
                permission_profile: Some("daily-wiki-agent".to_string()),
            }],
            dcr_enabled: true,
        })
        .unwrap();

        let user = issuer
            .user_for_subject("https://eric.wendland.dev/")
            .unwrap();
        assert_eq!(user.subject, "https://eric.wendland.dev");
        let token = issuer
            .issue_access_token_for("https://eric.wendland.dev/")
            .unwrap();
        let identity = issuer.validate_bearer_token(&token).unwrap();
        assert_eq!(identity.subject, "https://eric.wendland.dev/");
        assert_eq!(
            identity.permission_profile.as_deref(),
            Some("daily-wiki-agent")
        );
    }

    #[test]
    fn indieauth_link_header_discovery_prefers_requested_rel() {
        let value = r#"<https://example.test/metadata>; rel="indieauth-metadata", <https://example.test/auth>; rel="authorization_endpoint""#;
        assert_eq!(
            discover_link_header_value_rel(value, "indieauth-metadata").as_deref(),
            Some("https://example.test/metadata")
        );
        assert_eq!(
            discover_link_header_value_rel(value, "authorization_endpoint").as_deref(),
            Some("https://example.test/auth")
        );
    }

    #[test]
    fn indieauth_html_link_discovery_accepts_attribute_ordering() {
        let html = r#"
            <link href="/metadata" rel="indieauth-metadata">
            <link rel="authorization_endpoint" href="/auth">
            <link rel="token_endpoint" href="/token">
        "#;
        assert_eq!(
            discover_html_link_rel(html, "indieauth-metadata").as_deref(),
            Some("/metadata")
        );
        assert_eq!(
            discover_html_link_rel(html, "authorization_endpoint").as_deref(),
            Some("/auth")
        );
        assert_eq!(
            resolve_url("https://eric.wendland.dev/profile", "/auth").as_deref(),
            Some("https://eric.wendland.dev/auth")
        );
    }

    #[test]
    fn indieauth_network_urls_must_use_https() {
        assert!(parse_https_url("https://example.test/profile", "profile").is_ok());
        assert!(parse_https_url("http://example.test/profile", "profile")
            .unwrap_err()
            .to_string()
            .contains("must use HTTPS"));
        assert!(exchange_indieauth_code(
            "http://example.test/token",
            "code",
            "https://wiki.example.test/oauth/callback",
            "https://wiki.example.test",
            "verifier",
        )
        .unwrap_err()
        .to_string()
        .contains("must use HTTPS"));
    }

    #[test]
    fn client_id_metadata_rejects_non_public_urls_before_fetching() {
        for client_id in [
            "http://client.example.test/metadata.json",
            "https://localhost/metadata.json",
            "https://127.0.0.1/metadata.json",
            "https://[::1]/metadata.json",
            "https://client.local/metadata.json",
            "https://user@client.example.test/metadata.json",
        ] {
            assert!(fetch_client_id_metadata(client_id).is_err(), "{client_id}");
        }
        let metadata: ClientIdMetadataDocument = serde_json::from_value(serde_json::json!({
            "client_id": "https://client.example.test/metadata.json",
            "redirect_uris": ["https://client.example.test/callback"]
        }))
        .expect("metadata");
        assert_eq!(metadata.token_endpoint_auth_method, "none");
    }
}
