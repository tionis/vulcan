use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
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
pub struct OAuthResourceServer {
    issuer: String,
    audiences: Vec<String>,
    allowed_subs: BTreeSet<String>,
    allowed_emails: BTreeSet<String>,
    public_url: String,
    protected_resource_metadata_url: String,
    jwks: JwkSet,
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
        let issuer = trim_trailing_slash(&config.issuer);
        let jwks_url = match config.jwks_url.as_deref() {
            Some(url) => url.to_string(),
            None => discover_oidc_jwks_uri(&issuer)?,
        };
        let jwks = fetch_jwks(&jwks_url)?;
        let protected_resource_metadata_url = protected_resource_metadata_url(&config.public_url)?;
        Ok(Self {
            issuer,
            audiences: config.audiences,
            allowed_subs: config.allowed_subs.into_iter().collect(),
            allowed_emails: config.allowed_emails.into_iter().collect(),
            public_url: config.public_url,
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
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct OAuthClaims {
    sub: String,
    email: Option<String>,
}

fn discover_oidc_jwks_uri(issuer: &str) -> Result<String, OAuthError> {
    let discovery_url = format!("{issuer}/.well-known/openid-configuration");
    let document = reqwest::blocking::get(&discovery_url)
        .map_err(|error| OAuthError::Network(format!("failed to fetch OIDC discovery: {error}")))?
        .error_for_status()
        .map_err(|error| OAuthError::Network(format!("OIDC discovery failed: {error}")))?
        .json::<OidcDiscoveryDocument>()
        .map_err(|error| OAuthError::Network(format!("invalid OIDC discovery JSON: {error}")))?;
    Ok(document.jwks_uri)
}

fn fetch_jwks(jwks_url: &str) -> Result<JwkSet, OAuthError> {
    reqwest::blocking::get(jwks_url)
        .map_err(|error| OAuthError::Network(format!("failed to fetch OAuth JWKS: {error}")))?
        .error_for_status()
        .map_err(|error| OAuthError::Network(format!("OAuth JWKS fetch failed: {error}")))?
        .json::<JwkSet>()
        .map_err(|error| OAuthError::Network(format!("invalid OAuth JWKS JSON: {error}")))
}

fn trim_trailing_slash(value: &str) -> String {
    value.trim_end_matches('/').to_string()
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
}
