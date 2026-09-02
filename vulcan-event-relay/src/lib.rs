//! Transport-independent Event Relay Protocol and Git realtime-event models.
//!
//! This crate deliberately performs no network or filesystem I/O. It validates
//! untrusted descriptors, subscription bundles, `CloudEvents`, and Git profile
//! payloads before a caller persists credentials or schedules synchronization.

use std::collections::HashSet;
use std::fmt::{Debug, Display, Formatter};

use chrono::DateTime;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use url::Url;

pub const EVENT_RELAY_SPEC: &str = "event-relay/1";
pub const SUBSCRIPTION_SPEC: &str = "event-relay-subscription/1";
pub const GIT_PROFILE: &str = "https://tionis.dev/spec/git-realtime/1";
pub const GIT_REFS_UPDATED_TYPE: &str = "dev.tionis.git.refs.updated.v1";
pub const GIT_REF_STATE_TYPE: &str = "dev.tionis.git.ref.state.v1";
pub const DEFAULT_MAX_EVENT_BYTES: u64 = 256 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl Debug for SecretString {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

impl Serialize for SecretString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str("[REDACTED]")
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDescriptor {
    pub spec: String,
    pub id: String,
    pub profiles: Vec<String>,
    pub bindings: Vec<RelayBinding>,
    pub authorization: Vec<String>,
    pub retention: Vec<RetentionRule>,
    pub limits: RelayLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayBinding {
    #[serde(rename = "type")]
    pub kind: String,
    pub endpoint: String,
    pub subject_filter: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayLimits {
    pub event_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionRule {
    pub id: String,
    pub types: Vec<String>,
    #[serde(rename = "class")]
    pub class: RetentionClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_count: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    Ephemeral,
    BoundedLog,
    LatestBySubject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionBundle {
    pub spec: String,
    pub descriptor: SourceDescriptor,
    pub credential: SubscriberCredential,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriberCredential {
    pub scheme: String,
    pub token: SecretString,
}

impl Debug for SubscriberCredential {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubscriberCredential")
            .field("scheme", &self.scheme)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudEvent {
    pub specversion: String,
    pub id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub time: String,
    pub datacontenttype: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedGitEvent {
    pub id: String,
    pub source: String,
    pub event: GitEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitEvent {
    RefsUpdated(GitRefsUpdated),
    RefState(GitRefState),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitRefsUpdated {
    pub object_format: GitObjectFormat,
    #[serde(default)]
    pub atomic: bool,
    pub updates: Vec<GitRefUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitRefUpdate {
    #[serde(rename = "ref")]
    pub reference: String,
    pub before: Option<String>,
    pub after: Option<String>,
    #[serde(default)]
    pub forced: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitRefState {
    pub object_format: GitObjectFormat,
    #[serde(rename = "ref")]
    pub reference: String,
    pub oid: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitObjectFormat {
    Sha1,
    Sha256,
}

impl GitObjectFormat {
    const fn oid_length(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub code: &'static str,
    pub path: String,
    pub detail: String,
}

impl ValidationError {
    fn new(code: &'static str, path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            detail: detail.into(),
        }
    }
}

impl Display for ValidationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} at {}: {}", self.code, self.path, self.detail)
    }
}

impl std::error::Error for ValidationError {}

impl SourceDescriptor {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.spec != EVENT_RELAY_SPEC {
            return Err(ValidationError::new(
                "relay.unsupported-spec",
                "spec",
                format!("expected `{EVENT_RELAY_SPEC}`"),
            ));
        }
        validate_uri("id", &self.id)?;
        if self.profiles.is_empty() {
            return Err(ValidationError::new(
                "relay.missing-profile",
                "profiles",
                "at least one profile is required",
            ));
        }
        for (index, profile) in self.profiles.iter().enumerate() {
            validate_uri(&format!("profiles[{index}]"), profile)?;
        }
        if self.bindings.is_empty() {
            return Err(ValidationError::new(
                "relay.missing-binding",
                "bindings",
                "at least one binding is required",
            ));
        }
        for (index, binding) in self.bindings.iter().enumerate() {
            binding.validate(index)?;
        }
        if self.authorization.is_empty() {
            return Err(ValidationError::new(
                "relay.missing-authorization",
                "authorization",
                "at least one authorization scheme is required",
            ));
        }
        if self.limits.event_bytes == 0 || self.limits.event_bytes > DEFAULT_MAX_EVENT_BYTES {
            return Err(ValidationError::new(
                "relay.invalid-event-limit",
                "limits.event_bytes",
                format!("must be between 1 and {DEFAULT_MAX_EVENT_BYTES}"),
            ));
        }
        validate_retention(&self.retention)
    }
}

impl RelayBinding {
    fn validate(&self, index: usize) -> Result<(), ValidationError> {
        let root = format!("bindings[{index}]");
        if self.kind != "nats" {
            return Err(ValidationError::new(
                "relay.unsupported-binding",
                format!("{root}.type"),
                format!("unsupported binding `{}`", self.kind),
            ));
        }
        let endpoint = Url::parse(&self.endpoint).map_err(|error| {
            ValidationError::new(
                "relay.invalid-endpoint",
                format!("{root}.endpoint"),
                error.to_string(),
            )
        })?;
        if endpoint.scheme() != "tls" {
            return Err(ValidationError::new(
                "relay.insecure-endpoint",
                format!("{root}.endpoint"),
                "NATS endpoints must use `tls://`",
            ));
        }
        if endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ValidationError::new(
                "relay.invalid-endpoint",
                format!("{root}.endpoint"),
                "endpoint must contain only TLS scheme, host, and optional port",
            ));
        }
        if !valid_nats_subject_filter(&self.subject_filter) {
            return Err(ValidationError::new(
                "relay.invalid-subject-filter",
                format!("{root}.subject_filter"),
                "subject filter must contain safe tokens and end in `.>`",
            ));
        }
        Ok(())
    }
}

impl SubscriptionBundle {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.spec != SUBSCRIPTION_SPEC {
            return Err(ValidationError::new(
                "relay.unsupported-subscription-spec",
                "spec",
                format!("expected `{SUBSCRIPTION_SPEC}`"),
            ));
        }
        self.descriptor.validate()?;
        if self.credential.scheme != "bearer_capability" {
            return Err(ValidationError::new(
                "relay.unsupported-credential",
                "credential.scheme",
                "only `bearer_capability` is supported",
            ));
        }
        if !self
            .descriptor
            .authorization
            .iter()
            .any(|scheme| scheme == &self.credential.scheme)
        {
            return Err(ValidationError::new(
                "relay.credential-not-advertised",
                "credential.scheme",
                "descriptor does not advertise the supplied credential scheme",
            ));
        }
        if self.credential.token.expose_secret().len() < 43 {
            return Err(ValidationError::new(
                "relay.invalid-capability",
                "credential.token",
                "capability token is too short to carry 256 bits",
            ));
        }
        Ok(())
    }
}

pub fn validate_git_event(event: &CloudEvent) -> Result<ValidatedGitEvent, ValidationError> {
    validate_cloud_event(event)?;
    let parsed = match event.kind.as_str() {
        GIT_REFS_UPDATED_TYPE => {
            if event.subject.is_some() {
                return Err(ValidationError::new(
                    "git.unexpected-subject",
                    "subject",
                    "refs.updated does not define a subject",
                ));
            }
            let update: GitRefsUpdated = parse_data(event)?;
            validate_refs_updated(&update)?;
            GitEvent::RefsUpdated(update)
        }
        GIT_REF_STATE_TYPE => {
            let state: GitRefState = parse_data(event)?;
            validate_ref_state(event.subject.as_deref(), &state)?;
            GitEvent::RefState(state)
        }
        _ => {
            return Err(ValidationError::new(
                "git.unsupported-event-type",
                "type",
                "event type is not part of Git Realtime Events version 1",
            ));
        }
    };
    Ok(ValidatedGitEvent {
        id: event.id.clone(),
        source: event.source.clone(),
        event: parsed,
    })
}

fn validate_cloud_event(event: &CloudEvent) -> Result<(), ValidationError> {
    if event.specversion != "1.0" {
        return Err(ValidationError::new(
            "cloudevents.unsupported-version",
            "specversion",
            "expected CloudEvents 1.0",
        ));
    }
    if event.id.trim().is_empty() {
        return Err(ValidationError::new(
            "cloudevents.missing-id",
            "id",
            "event id must not be empty",
        ));
    }
    validate_uri("source", &event.source)?;
    if event.datacontenttype != "application/json" {
        return Err(ValidationError::new(
            "cloudevents.invalid-content-type",
            "datacontenttype",
            "expected `application/json`",
        ));
    }
    DateTime::parse_from_rfc3339(&event.time).map_err(|error| {
        ValidationError::new("cloudevents.invalid-time", "time", error.to_string())
    })?;
    Ok(())
}

fn parse_data<T: for<'de> Deserialize<'de>>(event: &CloudEvent) -> Result<T, ValidationError> {
    serde_json::from_value(event.data.clone())
        .map_err(|error| ValidationError::new("git.invalid-data", "data", error.to_string()))
}

fn validate_refs_updated(update: &GitRefsUpdated) -> Result<(), ValidationError> {
    if update.updates.is_empty() {
        return Err(ValidationError::new(
            "git.empty-update",
            "data.updates",
            "at least one ref update is required",
        ));
    }
    let mut references = HashSet::new();
    for (index, item) in update.updates.iter().enumerate() {
        let root = format!("data.updates[{index}]");
        validate_git_ref(&format!("{root}.ref"), &item.reference)?;
        if !references.insert(&item.reference) {
            return Err(ValidationError::new(
                "git.duplicate-ref",
                format!("{root}.ref"),
                "a ref may occur only once in an event",
            ));
        }
        if item.before.is_none() && item.after.is_none() {
            return Err(ValidationError::new(
                "git.empty-ref-update",
                root,
                "before and after cannot both be null",
            ));
        }
        if let Some(oid) = item.before.as_deref() {
            validate_oid(&format!("{root}.before"), oid, update.object_format)?;
        }
        if let Some(oid) = item.after.as_deref() {
            validate_oid(&format!("{root}.after"), oid, update.object_format)?;
        }
    }
    Ok(())
}

fn validate_ref_state(subject: Option<&str>, state: &GitRefState) -> Result<(), ValidationError> {
    validate_git_ref("data.ref", &state.reference)?;
    if subject != Some(state.reference.as_str()) {
        return Err(ValidationError::new(
            "git.subject-mismatch",
            "subject",
            "subject must exactly equal data.ref",
        ));
    }
    if let Some(oid) = state.oid.as_deref() {
        validate_oid("data.oid", oid, state.object_format)?;
    }
    Ok(())
}

fn validate_oid(
    path: &str,
    oid: &str,
    object_format: GitObjectFormat,
) -> Result<(), ValidationError> {
    if oid.len() != object_format.oid_length()
        || !oid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ValidationError::new(
            "git.invalid-oid",
            path,
            format!(
                "expected {} lowercase hexadecimal characters",
                object_format.oid_length()
            ),
        ));
    }
    Ok(())
}

fn validate_uri(path: &str, value: &str) -> Result<(), ValidationError> {
    let url = Url::parse(value)
        .map_err(|error| ValidationError::new("relay.invalid-uri", path, error.to_string()))?;
    if url.cannot_be_a_base() && url.scheme() != "urn" {
        return Err(ValidationError::new(
            "relay.invalid-uri",
            path,
            "URI must be hierarchical or use the `urn` scheme",
        ));
    }
    Ok(())
}

fn validate_retention(rules: &[RetentionRule]) -> Result<(), ValidationError> {
    if rules.is_empty() {
        return Err(ValidationError::new(
            "relay.missing-retention",
            "retention",
            "at least one retention rule is required",
        ));
    }
    let mut ids = HashSet::new();
    let mut types = HashSet::new();
    for (index, rule) in rules.iter().enumerate() {
        let root = format!("retention[{index}]");
        if !valid_rule_id(&rule.id) || !ids.insert(&rule.id) {
            return Err(ValidationError::new(
                "relay.invalid-retention-id",
                format!("{root}.id"),
                "rule ids must be unique lowercase letters, digits, or hyphens",
            ));
        }
        if rule.types.is_empty() {
            return Err(ValidationError::new(
                "relay.empty-retention-types",
                format!("{root}.types"),
                "at least one event type is required",
            ));
        }
        for event_type in &rule.types {
            if event_type == "*" {
                if index + 1 != rules.len() || rule.class != RetentionClass::Ephemeral {
                    return Err(ValidationError::new(
                        "relay.invalid-retention-wildcard",
                        format!("{root}.types"),
                        "a wildcard is allowed only in the final ephemeral rule",
                    ));
                }
            } else if !types.insert(event_type) {
                return Err(ValidationError::new(
                    "relay.overlapping-retention",
                    format!("{root}.types"),
                    format!("event type `{event_type}` occurs in more than one rule"),
                ));
            }
        }
        if rule.class == RetentionClass::BoundedLog
            && rule.max_age_seconds.is_none()
            && rule.max_count.is_none()
        {
            return Err(ValidationError::new(
                "relay.unbounded-log",
                root,
                "bounded_log requires max_age_seconds or max_count",
            ));
        }
    }
    Ok(())
}

fn valid_rule_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_nats_subject_filter(value: &str) -> bool {
    let tokens = value.split('.').collect::<Vec<_>>();
    tokens.len() >= 2
        && tokens.last() == Some(&">")
        && tokens[..tokens.len() - 1].iter().all(|token| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}

fn validate_git_ref(path: &str, reference: &str) -> Result<(), ValidationError> {
    let invalid_byte = reference.bytes().any(|byte| {
        byte <= b' '
            || byte == 0x7f
            || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    });
    let invalid_component = reference.split('/').any(|component| {
        component.is_empty()
            || component.starts_with('.')
            || component.as_bytes().ends_with(b".lock")
    });
    if !reference.starts_with("refs/")
        || reference.ends_with('.')
        || reference.contains("..")
        || reference.contains("@{")
        || invalid_byte
        || invalid_component
    {
        return Err(ValidationError::new(
            "git.invalid-ref",
            path,
            "expected a complete valid Git reference name",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_BUNDLE: &str = r#"{
      "spec":"event-relay-subscription/1",
      "descriptor":{
        "spec":"event-relay/1",
        "id":"urn:event-relay-channel:01K00000000000000000000000",
        "profiles":["https://tionis.dev/spec/git-realtime/1"],
        "bindings":[{"type":"nats","endpoint":"tls://events.example.net:4222","subject_filter":"events.channels.01K00000000000000000000000.>"}],
        "authorization":["bearer_capability"],
        "retention":[
          {"id":"git-updates","types":["dev.tionis.git.refs.updated.v1"],"class":"bounded_log","max_age_seconds":86400},
          {"id":"git-state","types":["dev.tionis.git.ref.state.v1"],"class":"latest_by_subject"}
        ],
        "limits":{"event_bytes":65536},
        "future_field":true
      },
      "credential":{"scheme":"bearer_capability","token":"er1.client.0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"}
    }"#;

    fn event(kind: &str, subject: Option<&str>, data: Value) -> CloudEvent {
        CloudEvent {
            specversion: "1.0".to_string(),
            id: "01K00000000000000000000000".to_string(),
            source: "urn:git-repository:01K00000000000000000000000".to_string(),
            kind: kind.to_string(),
            subject: subject.map(str::to_string),
            time: "2026-09-02T20:00:00Z".to_string(),
            datacontenttype: "application/json".to_string(),
            data,
        }
    }

    #[test]
    fn valid_bundle_accepts_additive_fields_and_redacts_secrets() {
        let bundle: SubscriptionBundle = serde_json::from_str(VALID_BUNDLE).expect("bundle");
        bundle.validate().expect("valid bundle");
        assert!(!format!("{bundle:?}").contains(bundle.credential.token.expose_secret()));
        let serialized = serde_json::to_string(&bundle).expect("redacted JSON");
        assert!(serialized.contains("[REDACTED]"));
        assert!(!serialized.contains("er1.client."));
    }

    #[test]
    fn bundle_rejects_credentials_in_endpoints_and_short_tokens() {
        let mut bundle: SubscriptionBundle = serde_json::from_str(VALID_BUNDLE).expect("bundle");
        bundle.descriptor.bindings[0].endpoint = "tls://secret@events.example.net:4222".to_string();
        assert_eq!(
            bundle.validate().expect_err("userinfo").code,
            "relay.invalid-endpoint"
        );
        bundle.descriptor.bindings[0].endpoint = "tls://events.example.net:4222".to_string();
        bundle.credential.token = SecretString::new("too-short");
        assert_eq!(
            bundle.validate().expect_err("short token").code,
            "relay.invalid-capability"
        );
    }

    #[test]
    fn refs_updated_validates_oids_refs_and_uniqueness() {
        let oid = "89abcdef0123456789abcdef0123456789abcdef";
        let valid = event(
            GIT_REFS_UPDATED_TYPE,
            None,
            serde_json::json!({
                "object_format":"sha1",
                "atomic":false,
                "updates":[{"ref":"refs/heads/main","before":null,"after":oid}]
            }),
        );
        assert!(matches!(
            validate_git_event(&valid).expect("valid event").event,
            GitEvent::RefsUpdated(_)
        ));

        let mut duplicate = valid;
        duplicate.data["updates"] = serde_json::json!([
            {"ref":"refs/heads/main","before":null,"after":oid},
            {"ref":"refs/heads/main","before":oid,"after":null}
        ]);
        assert_eq!(
            validate_git_event(&duplicate)
                .expect_err("duplicate ref")
                .code,
            "git.duplicate-ref"
        );
    }

    #[test]
    fn ref_state_requires_matching_subject_and_lowercase_full_oid() {
        let valid = event(
            GIT_REF_STATE_TYPE,
            Some("refs/heads/main"),
            serde_json::json!({
                "object_format":"sha256",
                "ref":"refs/heads/main",
                "oid":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            }),
        );
        assert!(matches!(
            validate_git_event(&valid).expect("valid state").event,
            GitEvent::RefState(_)
        ));

        let mut mismatch = valid;
        mismatch.subject = Some("refs/heads/other".to_string());
        assert_eq!(
            validate_git_event(&mismatch)
                .expect_err("subject mismatch")
                .code,
            "git.subject-mismatch"
        );
    }

    #[test]
    fn retention_rules_reject_order_dependent_overlaps() {
        let mut bundle: SubscriptionBundle = serde_json::from_str(VALID_BUNDLE).expect("bundle");
        bundle.descriptor.retention.push(RetentionRule {
            id: "duplicate".to_string(),
            types: vec![GIT_REFS_UPDATED_TYPE.to_string()],
            class: RetentionClass::Ephemeral,
            max_age_seconds: None,
            max_count: None,
        });
        assert_eq!(
            bundle.validate().expect_err("overlap").code,
            "relay.overlapping-retention"
        );
    }
}
