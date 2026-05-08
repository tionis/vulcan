use base64::prelude::{Engine, BASE64_URL_SAFE_NO_PAD};
use jsonwebtoken::{
    decode, decode_header, encode, jwk::JwkSet, Algorithm, DecodingKey, EncodingKey, Header,
    Validation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

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
    pub approval_token: String,
    pub subject: String,
    pub email: Option<String>,
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
    approval_token: String,
    subject: String,
    email: Option<String>,
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

    pub fn validate_bearer_token(&self, token: &str) -> Result<(), OAuthError> {
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
        if self.allowed_subs.contains(&claims.sub) {
            return Ok(());
        }
        if claims
            .email
            .as_deref()
            .is_some_and(|email| self.allowed_emails.contains(email))
        {
            return Ok(());
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
            || config.approval_token.is_empty()
            || config.subject.is_empty()
        {
            return Err(OAuthError::Config(
                "local OAuth issuer requires non-empty client id, client secret, approval token, and subject"
                    .to_string(),
            ));
        }
        let protected_resource_metadata_url = protected_resource_metadata_url(&config.public_url)?;
        let origin = public_url_origin(&config.public_url)?;
        let authorization_server_metadata = serde_json::json!({
            "issuer": config.public_url,
            "authorization_endpoint": format!("{origin}/oauth/authorize"),
            "token_endpoint": format!("{origin}/oauth/token"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code"],
            "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
            "code_challenge_methods_supported": ["S256"],
            "scopes_supported": ["openid", "email", "profile"],
        });
        Ok(Self {
            public_url: config.public_url,
            client_id: config.client_id,
            client_secret: config.client_secret,
            approval_token: config.approval_token,
            subject: config.subject,
            email: config.email,
            protected_resource_metadata_url,
            authorization_server_metadata,
        })
    }

    pub fn validate_bearer_token(&self, token: &str) -> Result<(), OAuthError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[self.public_url.as_str()]);
        validation.set_audience(&[self.public_url.as_str()]);
        validation.leeway = 60;
        let token = decode::<LocalOAuthClaims>(
            token,
            &DecodingKey::from_secret(self.client_secret.as_bytes()),
            &validation,
        )
        .map_err(|error| OAuthError::Token(format!("invalid local OAuth bearer token: {error}")))?;
        if token.claims.sub == self.subject {
            Ok(())
        } else {
            Err(OAuthError::Token(
                "local OAuth token subject is not allowed".to_string(),
            ))
        }
    }

    pub fn issue_access_token(&self) -> Result<String, OAuthError> {
        let now = unix_timestamp();
        let claims = LocalOAuthClaims {
            iss: self.public_url.clone(),
            sub: self.subject.clone(),
            aud: vec![self.public_url.clone()],
            exp: now + 3600,
            iat: now,
            email: self.email.clone(),
        };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.client_secret.as_bytes()),
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
    fn local_oauth_issuer_issues_and_validates_tokens() {
        let issuer = LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: "https://wiki.example.test/mcp".to_string(),
            client_id: "vulcan-mcp".to_string(),
            client_secret: "secret".to_string(),
            approval_token: "approve".to_string(),
            subject: "eric".to_string(),
            email: Some("eric@example.test".to_string()),
        })
        .unwrap();
        assert!(issuer.verify_client("vulcan-mcp", "secret"));
        assert!(issuer.verify_approval_token("approve"));
        let token = issuer.issue_access_token().unwrap();
        issuer.validate_bearer_token(&token).unwrap();
        assert_eq!(
            issuer.authorization_server_metadata()["issuer"],
            issuer.public_url()
        );
    }
}
