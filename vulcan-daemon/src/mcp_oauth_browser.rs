//! Bounded, single-use browser transactions for local MCP OAuth issuers.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::prelude::{Engine as _, BASE64_URL_SAFE_NO_PAD};
use subtle::ConstantTimeEq;

const MAX_PENDING_TRANSACTIONS: usize = 256;
const TRANSACTION_LIFETIME: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct PendingIndieAuth {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub scopes: Vec<String>,
    pub resource: String,
    pub indieauth_code_verifier: String,
    pub state: Option<String>,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct PendingConsent {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub subject: String,
    pub scopes: Vec<String>,
    pub resource: String,
    pub state: Option<String>,
    pub csrf_token: String,
    pub expires_at: Instant,
}

pub type PendingIndieAuthMap = Mutex<BTreeMap<String, PendingIndieAuth>>;
pub type PendingConsentMap = Mutex<BTreeMap<String, PendingConsent>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginError {
    Capacity,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeError {
    Unknown,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentError {
    Unknown,
    Expired,
    InvalidCsrf,
    InvalidDecision,
}

fn random_token() -> Result<String, BeginError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| BeginError::Random)?;
    Ok(BASE64_URL_SAFE_NO_PAD.encode(bytes))
}

pub fn begin_indieauth(
    pending: &PendingIndieAuthMap,
    mut transaction: PendingIndieAuth,
) -> Result<String, BeginError> {
    let mut entries = pending.lock().expect("pending IndieAuth lock poisoned");
    entries.retain(|_, entry| entry.expires_at >= Instant::now());
    if entries.len() >= MAX_PENDING_TRANSACTIONS {
        return Err(BeginError::Capacity);
    }
    let state = random_token()?;
    transaction.expires_at = Instant::now() + TRANSACTION_LIFETIME;
    entries.insert(state.clone(), transaction);
    Ok(state)
}

pub fn take_indieauth(
    pending: &PendingIndieAuthMap,
    state: &str,
) -> Result<PendingIndieAuth, TakeError> {
    let transaction = pending
        .lock()
        .expect("pending IndieAuth lock poisoned")
        .remove(state)
        .ok_or(TakeError::Unknown)?;
    if transaction.expires_at < Instant::now() {
        return Err(TakeError::Expired);
    }
    Ok(transaction)
}

pub fn begin_consent(
    pending: &PendingConsentMap,
    mut transaction: PendingConsent,
) -> Result<(String, PendingConsent), BeginError> {
    let mut entries = pending.lock().expect("pending consent lock poisoned");
    entries.retain(|_, entry| entry.expires_at >= Instant::now());
    if entries.len() >= MAX_PENDING_TRANSACTIONS {
        return Err(BeginError::Capacity);
    }
    let id = random_token()?;
    transaction.csrf_token = random_token()?;
    transaction.expires_at = Instant::now() + TRANSACTION_LIFETIME;
    entries.insert(id.clone(), transaction.clone());
    Ok((id, transaction))
}

pub fn consume_consent(
    pending: &PendingConsentMap,
    id: &str,
    csrf: &str,
    decision: &str,
) -> Result<PendingConsent, ConsentError> {
    let mut entries = pending.lock().expect("pending consent lock poisoned");
    let transaction = entries.get(id).ok_or(ConsentError::Unknown)?;
    if transaction.expires_at < Instant::now() {
        entries.remove(id);
        return Err(ConsentError::Expired);
    }
    if transaction
        .csrf_token
        .as_bytes()
        .ct_eq(csrf.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(ConsentError::InvalidCsrf);
    }
    if !matches!(decision, "approve" | "deny") {
        return Err(ConsentError::InvalidDecision);
    }
    Ok(entries.remove(id).expect("validated consent exists"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn consent() -> PendingConsent {
        PendingConsent {
            client_id: "client".into(),
            redirect_uri: "https://client.example/callback".into(),
            code_challenge: "challenge".into(),
            subject: "https://user.example/".into(),
            scopes: vec!["mcp:tools".into()],
            resource: "https://remote.example/mcp".into(),
            state: None,
            csrf_token: String::new(),
            expires_at: Instant::now(),
        }
    }

    fn indieauth() -> PendingIndieAuth {
        PendingIndieAuth {
            client_id: "client".into(),
            redirect_uri: "https://client.example/callback".into(),
            code_challenge: "challenge".into(),
            scopes: vec!["mcp:tools".into()],
            resource: "https://remote.example/mcp".into(),
            indieauth_code_verifier: "verifier".into(),
            state: Some("original-client-state".into()),
            expires_at: Instant::now(),
        }
    }

    #[test]
    fn indieauth_state_is_random_bounded_and_single_use() {
        let map = PendingIndieAuthMap::default();
        let state = begin_indieauth(&map, indieauth()).unwrap();
        assert_eq!(state.len(), 43);
        assert_eq!(
            take_indieauth(&map, &state).unwrap().state.as_deref(),
            Some("original-client-state")
        );
        assert!(matches!(
            take_indieauth(&map, &state),
            Err(TakeError::Unknown)
        ));
        for _ in 0..MAX_PENDING_TRANSACTIONS {
            begin_indieauth(&map, indieauth()).unwrap();
        }
        assert_eq!(
            begin_indieauth(&map, indieauth()).unwrap_err(),
            BeginError::Capacity
        );
        let first = map.lock().unwrap().keys().next().unwrap().clone();
        map.lock().unwrap().get_mut(&first).unwrap().expires_at =
            Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            take_indieauth(&map, &first),
            Err(TakeError::Expired)
        ));
        assert!(begin_indieauth(&map, indieauth()).is_ok());
    }

    #[test]
    fn consent_is_random_bounded_and_single_use() {
        let map = PendingConsentMap::default();
        let (id, record) = begin_consent(&map, consent()).unwrap();
        assert_eq!(id.len(), 43);
        assert_eq!(record.csrf_token.len(), 43);
        assert_ne!(id, record.csrf_token);
        assert!(matches!(
            consume_consent(&map, &id, "wrong", "approve"),
            Err(ConsentError::InvalidCsrf)
        ));
        assert!(matches!(
            consume_consent(&map, &id, &record.csrf_token, "other"),
            Err(ConsentError::InvalidDecision)
        ));
        assert!(consume_consent(&map, &id, &record.csrf_token, "deny").is_ok());
        assert!(matches!(
            consume_consent(&map, &id, &record.csrf_token, "approve"),
            Err(ConsentError::Unknown)
        ));
    }

    #[test]
    fn concurrent_approval_consumes_at_most_once() {
        let map = Arc::new(PendingConsentMap::default());
        let (id, record) = begin_consent(&map, consent()).unwrap();
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let map = Arc::clone(&map);
                let id = id.clone();
                let csrf = record.csrf_token.clone();
                std::thread::spawn(move || consume_consent(&map, &id, &csrf, "approve").is_ok())
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|approved| *approved)
                .count(),
            1
        );
    }

    #[test]
    fn capacity_reclaims_expired_transactions() {
        let map = PendingConsentMap::default();
        for _ in 0..MAX_PENDING_TRANSACTIONS {
            begin_consent(&map, consent()).unwrap();
        }
        assert_eq!(
            begin_consent(&map, consent()).unwrap_err(),
            BeginError::Capacity
        );
        let first = map.lock().unwrap().keys().next().unwrap().clone();
        map.lock().unwrap().get_mut(&first).unwrap().expires_at =
            Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert!(begin_consent(&map, consent()).is_ok());
    }
}
