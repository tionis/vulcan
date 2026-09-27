//! Ephemeral one-time OAuth authorization codes for MCP listeners.

use base64::prelude::{Engine, BASE64_URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn pkce_s256_challenge(verifier: &str) -> String {
    BASE64_URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub const MAX_PENDING_OAUTH_CODES: usize = 256;
pub const OAUTH_CODE_LIFETIME: Duration = Duration::from_secs(300);
const CODE_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthorizationCode {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub subject: String,
    pub scopes: Vec<String>,
    pub resource: String,
    pub grant_id: Option<String>,
    pub grant_required: bool,
    pub expires_at: Instant,
}

pub type McpAuthorizationCodeMap = Mutex<BTreeMap<String, McpAuthorizationCode>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCodeIssueError {
    Capacity,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCodeRedeemError {
    Unknown,
    Expired,
    ClientMismatch,
    RedirectMismatch,
    InvalidVerifier,
}

impl McpCodeRedeemError {
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Unknown => "unknown authorization code",
            Self::Expired => "expired authorization code",
            Self::ClientMismatch => "authorization code client mismatch",
            Self::RedirectMismatch => "authorization code redirect mismatch",
            Self::InvalidVerifier => "invalid PKCE verifier",
        }
    }
}

/// Issue a bounded, one-time code after consent has established the record's authority.
pub fn issue_mcp_authorization_code(
    codes: &McpAuthorizationCodeMap,
    mut record: McpAuthorizationCode,
) -> Result<String, McpCodeIssueError> {
    let now = Instant::now();
    let mut pending = codes
        .lock()
        .expect("OAuth code lock should not be poisoned");
    pending.retain(|_, existing| existing.expires_at > now);
    if pending.len() >= MAX_PENDING_OAUTH_CODES {
        return Err(McpCodeIssueError::Capacity);
    }
    let mut bytes = [0_u8; CODE_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| McpCodeIssueError::Random)?;
    let code = BASE64_URL_SAFE_NO_PAD.encode(bytes);
    record.expires_at = now + OAUTH_CODE_LIFETIME;
    pending.insert(code.clone(), record);
    Ok(code)
}

/// Attach the durable grant before the code is returned to the OAuth client.
pub fn bind_mcp_authorization_code_grant(
    codes: &McpAuthorizationCodeMap,
    code: &str,
    grant_id: String,
) -> bool {
    let mut pending = codes
        .lock()
        .expect("OAuth code lock should not be poisoned");
    let Some(record) = pending.get_mut(code) else {
        return false;
    };
    if record.expires_at <= Instant::now() || record.grant_id.is_some() || !record.grant_required {
        pending.remove(code);
        return false;
    }
    record.grant_id = Some(grant_id);
    true
}

/// Drop an undisclosed code when consent cannot create a durable grant.
pub fn discard_mcp_authorization_code(codes: &McpAuthorizationCodeMap, code: &str) {
    codes
        .lock()
        .expect("OAuth code lock should not be poisoned")
        .remove(code);
}

/// Consume the code before checking its binding, so a failed redemption cannot be replayed.
pub fn redeem_mcp_authorization_code(
    codes: &McpAuthorizationCodeMap,
    code: &str,
    client_id: &str,
    redirect_uri: Option<&str>,
    code_verifier: &str,
) -> Result<McpAuthorizationCode, McpCodeRedeemError> {
    let record = codes
        .lock()
        .expect("OAuth code lock should not be poisoned")
        .remove(code)
        .ok_or(McpCodeRedeemError::Unknown)?;
    if record.expires_at < Instant::now() {
        return Err(McpCodeRedeemError::Expired);
    }
    if record.grant_required && record.grant_id.is_none() {
        return Err(McpCodeRedeemError::Unknown);
    }
    if record.client_id != client_id {
        return Err(McpCodeRedeemError::ClientMismatch);
    }
    if Some(record.redirect_uri.as_str()) != redirect_uri {
        return Err(McpCodeRedeemError::RedirectMismatch);
    }
    if pkce_s256_challenge(code_verifier) != record.code_challenge {
        return Err(McpCodeRedeemError::InvalidVerifier);
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> McpAuthorizationCode {
        McpAuthorizationCode {
            client_id: "client".to_string(),
            redirect_uri: "https://client.example.test/callback".to_string(),
            code_challenge: pkce_s256_challenge("verifier"),
            subject: "https://identity.example.test/me".to_string(),
            scopes: vec!["mcp:tools".to_string()],
            resource: "https://mcp.example.test/mcp".to_string(),
            grant_id: None,
            grant_required: false,
            expires_at: Instant::now(),
        }
    }

    #[test]
    fn codes_are_random_one_time_and_bound_to_client_redirect_and_pkce() {
        let codes = McpAuthorizationCodeMap::default();
        let code = issue_mcp_authorization_code(&codes, record()).expect("code");
        assert_eq!(
            BASE64_URL_SAFE_NO_PAD.decode(&code).expect("base64").len(),
            CODE_BYTES
        );
        assert_eq!(
            redeem_mcp_authorization_code(
                &codes,
                &code,
                "other",
                Some("https://client.example.test/callback"),
                "verifier",
            ),
            Err(McpCodeRedeemError::ClientMismatch)
        );
        assert_eq!(
            redeem_mcp_authorization_code(
                &codes,
                &code,
                "client",
                Some("https://client.example.test/callback"),
                "verifier",
            ),
            Err(McpCodeRedeemError::Unknown)
        );
        let code = issue_mcp_authorization_code(&codes, record()).expect("new code");
        assert_eq!(
            redeem_mcp_authorization_code(
                &codes,
                &code,
                "client",
                Some("https://client.example.test/other"),
                "verifier",
            ),
            Err(McpCodeRedeemError::RedirectMismatch)
        );
        let code = issue_mcp_authorization_code(&codes, record()).expect("new code");
        assert_eq!(
            redeem_mcp_authorization_code(
                &codes,
                &code,
                "client",
                Some("https://client.example.test/callback"),
                "wrong",
            ),
            Err(McpCodeRedeemError::InvalidVerifier)
        );
        let code = issue_mcp_authorization_code(&codes, record()).expect("new code");
        assert!(redeem_mcp_authorization_code(
            &codes,
            &code,
            "client",
            Some("https://client.example.test/callback"),
            "verifier",
        )
        .is_ok());
    }

    #[test]
    fn expired_codes_are_reclaimed_before_capacity_admission() {
        let codes = McpAuthorizationCodeMap::default();
        {
            let mut pending = codes.lock().expect("codes");
            for index in 0..MAX_PENDING_OAUTH_CODES {
                let mut value = record();
                value.expires_at = Instant::now() + Duration::from_secs(60);
                pending.insert(format!("pending-{index}"), value);
            }
        }
        assert_eq!(
            issue_mcp_authorization_code(&codes, record()),
            Err(McpCodeIssueError::Capacity)
        );
        codes
            .lock()
            .expect("codes")
            .get_mut("pending-0")
            .expect("code")
            .expires_at = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("clock has at least one second of monotonic history");
        assert!(issue_mcp_authorization_code(&codes, record()).is_ok());
        assert_eq!(codes.lock().expect("codes").len(), MAX_PENDING_OAUTH_CODES);
    }

    #[test]
    fn expired_redemption_consumes_the_code() {
        let codes = McpAuthorizationCodeMap::default();
        codes.lock().expect("codes").insert(
            "expired".to_string(),
            McpAuthorizationCode {
                expires_at: Instant::now()
                    .checked_sub(Duration::from_secs(1))
                    .expect("clock has at least one second of monotonic history"),
                ..record()
            },
        );
        assert_eq!(
            redeem_mcp_authorization_code(
                &codes,
                "expired",
                "client",
                Some("https://client.example.test/callback"),
                "verifier"
            ),
            Err(McpCodeRedeemError::Expired)
        );
        assert!(codes.lock().expect("codes").is_empty());
    }

    #[test]
    fn undisclosed_code_can_be_bound_once_or_discarded_after_failed_consent() {
        let codes = McpAuthorizationCodeMap::default();
        let provisional = issue_mcp_authorization_code(
            &codes,
            McpAuthorizationCode {
                grant_required: true,
                ..record()
            },
        )
        .expect("provisional code");
        assert_eq!(
            redeem_mcp_authorization_code(
                &codes,
                &provisional,
                "client",
                Some("https://client.example.test/callback"),
                "verifier",
            ),
            Err(McpCodeRedeemError::Unknown)
        );
        let first = issue_mcp_authorization_code(
            &codes,
            McpAuthorizationCode {
                grant_required: true,
                ..record()
            },
        )
        .expect("code");
        assert!(bind_mcp_authorization_code_grant(
            &codes,
            &first,
            "grant-a".to_string()
        ));
        assert!(!bind_mcp_authorization_code_grant(
            &codes,
            &first,
            "grant-b".to_string()
        ));
        assert!(codes.lock().expect("codes").get(&first).is_none());
        let second = issue_mcp_authorization_code(
            &codes,
            McpAuthorizationCode {
                grant_required: true,
                ..record()
            },
        )
        .expect("second code");
        discard_mcp_authorization_code(&codes, &second);
        assert!(codes.lock().expect("codes").get(&second).is_none());
    }
}
