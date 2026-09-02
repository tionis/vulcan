//! Strict routing from untrusted relay events to ordinary sync jobs.

use crate::notifications::{
    NotificationStore, NotificationStoreError, NotificationSubscription, NotificationSubscriptionId,
};
use crate::registry::{RegistryError, WikiRegistry};
use crate::supervisor::{SupervisorError, SyncSupervisor};
use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};
use vulcan_event_relay::{validate_git_event, CloudEvent, GitEvent, ValidationError};
use vulcan_sync::SyncJobTrigger;

const MAX_DEDUPLICATION_KEYS: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationDisposition {
    Enqueued,
    Duplicate,
    IgnoredRef,
    InactiveBinding,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NotificationRouteReport {
    pub subscription_id: NotificationSubscriptionId,
    pub disposition: NotificationDisposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matching_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub coalesced: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Default)]
struct DeduplicationWindow {
    order: VecDeque<(String, String)>,
    keys: HashSet<(String, String)>,
}

impl DeduplicationWindow {
    fn contains(&self, source: &str, id: &str) -> bool {
        self.keys.contains(&(source.to_string(), id.to_string()))
    }

    fn insert(&mut self, source: String, id: String) {
        let key = (source, id);
        if !self.keys.insert(key.clone()) {
            return;
        }
        self.order.push_back(key);
        while self.order.len() > MAX_DEDUPLICATION_KEYS {
            if let Some(expired) = self.order.pop_front() {
                self.keys.remove(&expired);
            }
        }
    }
}

pub struct NotificationRouter {
    store: NotificationStore,
    registry: WikiRegistry,
    supervisor: Arc<SyncSupervisor>,
    seen: Mutex<DeduplicationWindow>,
}

impl NotificationRouter {
    #[must_use]
    pub fn new(
        store: NotificationStore,
        registry: WikiRegistry,
        supervisor: Arc<SyncSupervisor>,
    ) -> Self {
        Self {
            store,
            registry,
            supervisor,
            seen: Mutex::new(DeduplicationWindow::default()),
        }
    }

    pub fn route(
        &self,
        subscription_id: NotificationSubscriptionId,
        payload: &[u8],
    ) -> Result<NotificationRouteReport, NotificationRouterError> {
        let subscription = self.store.show(subscription_id)?;
        if payload.len() as u64 > subscription.descriptor.limits.event_bytes {
            return Ok(rejected(
                subscription_id,
                "relay.event-too-large",
                "event exceeds the subscription's advertised byte limit",
            ));
        }
        let Ok(event) = serde_json::from_slice::<CloudEvent>(payload) else {
            return Ok(rejected(
                subscription_id,
                "cloudevents.invalid-json",
                "event is not a structured JSON CloudEvent",
            ));
        };
        if event.source != subscription.source {
            return Ok(rejected(
                subscription_id,
                "git.source-mismatch",
                "event source does not match the explicit subscription binding",
            ));
        }
        let event = match validate_git_event(&event) {
            Ok(event) => event,
            Err(error) => return Ok(rejected_validation(subscription_id, &error)),
        };
        let matching_refs = matching_refs(&subscription, &event.event);
        if matching_refs.is_empty() {
            return Ok(NotificationRouteReport {
                subscription_id,
                disposition: NotificationDisposition::IgnoredRef,
                event_id: Some(event.id),
                matching_refs,
                job_id: None,
                coalesced: false,
                diagnostic_code: None,
                detail: None,
            });
        }
        {
            let seen = self
                .seen
                .lock()
                .map_err(|_| NotificationRouterError::Poisoned)?;
            if seen.contains(&event.source, &event.id) {
                return Ok(NotificationRouteReport {
                    subscription_id,
                    disposition: NotificationDisposition::Duplicate,
                    event_id: Some(event.id),
                    matching_refs,
                    job_id: None,
                    coalesced: true,
                    diagnostic_code: None,
                    detail: None,
                });
            }
        }
        let registration = match self.registry.show(&subscription.wiki_id) {
            Ok(status) => status.registration,
            Err(RegistryError::UnknownWiki(_)) => {
                return Ok(inactive(subscription_id, event.id, matching_refs));
            }
            Err(error) => return Err(error.into()),
        };
        if registration.registration_id != subscription.registration_id
            || registration.sync_paused
            || registration.sync_backend.as_deref() != Some("git")
        {
            return Ok(inactive(subscription_id, event.id, matching_refs));
        }
        let enqueued = self.supervisor.enqueue(
            registration.id.as_str(),
            &registration.path,
            SyncJobTrigger::RemoteNotification,
        )?;
        self.seen
            .lock()
            .map_err(|_| NotificationRouterError::Poisoned)?
            .insert(event.source, event.id.clone());
        Ok(NotificationRouteReport {
            subscription_id,
            disposition: NotificationDisposition::Enqueued,
            event_id: Some(event.id),
            matching_refs,
            job_id: Some(enqueued.job.job.id),
            coalesced: enqueued.coalesced,
            diagnostic_code: None,
            detail: None,
        })
    }
}

fn matching_refs(subscription: &NotificationSubscription, event: &GitEvent) -> Vec<String> {
    let candidates = match event {
        GitEvent::RefsUpdated(updated) => updated
            .updates
            .iter()
            .map(|update| update.reference.as_str())
            .collect::<Vec<_>>(),
        GitEvent::RefState(state) => vec![state.reference.as_str()],
    };
    subscription
        .refs
        .iter()
        .filter(|reference| candidates.contains(&reference.as_str()))
        .cloned()
        .collect()
}

fn rejected(
    subscription_id: NotificationSubscriptionId,
    code: &str,
    detail: &str,
) -> NotificationRouteReport {
    NotificationRouteReport {
        subscription_id,
        disposition: NotificationDisposition::Rejected,
        event_id: None,
        matching_refs: Vec::new(),
        job_id: None,
        coalesced: false,
        diagnostic_code: Some(code.to_string()),
        detail: Some(detail.to_string()),
    }
}

fn rejected_validation(
    subscription_id: NotificationSubscriptionId,
    error: &ValidationError,
) -> NotificationRouteReport {
    rejected(subscription_id, error.code, &error.detail)
}

fn inactive(
    subscription_id: NotificationSubscriptionId,
    event_id: String,
    matching_refs: Vec<String>,
) -> NotificationRouteReport {
    NotificationRouteReport {
        subscription_id,
        disposition: NotificationDisposition::InactiveBinding,
        event_id: Some(event_id),
        matching_refs,
        job_id: None,
        coalesced: false,
        diagnostic_code: Some("git.inactive-binding".to_string()),
        detail: Some(
            "bound wiki is missing, replaced, paused, or no longer uses Git sync".to_string(),
        ),
    }
}

#[derive(Debug)]
pub enum NotificationRouterError {
    Store(NotificationStoreError),
    Registry(RegistryError),
    Supervisor(SupervisorError),
    Poisoned,
}

impl Display for NotificationRouterError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => Display::fmt(error, formatter),
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Supervisor(error) => Display::fmt(error, formatter),
            Self::Poisoned => formatter.write_str("notification deduplication state is poisoned"),
        }
    }
}

impl Error for NotificationRouterError {}

impl From<NotificationStoreError> for NotificationRouterError {
    fn from(error: NotificationStoreError) -> Self {
        Self::Store(error)
    }
}

impl From<RegistryError> for NotificationRouterError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<SupervisorError> for NotificationRouterError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::NotificationStore;
    use crate::registry::{AddWikiRequest, WikiId};
    use tempfile::TempDir;
    use vulcan_event_relay::{SubscriptionBundle, GIT_PROFILE};

    struct Fixture {
        _temporary: TempDir,
        router: NotificationRouter,
        supervisor: Arc<SyncSupervisor>,
        subscription_id: NotificationSubscriptionId,
        source: String,
    }

    fn fixture() -> Fixture {
        let temporary = TempDir::new().expect("temporary directory");
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let registration = registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse("notes").expect("wiki ID"),
                    path: vault,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("registration");
        let store = NotificationStore::at(temporary.path());
        let source = "urn:git-repository:01K00000000000000000000000".to_string();
        let bundle: SubscriptionBundle = serde_json::from_value(serde_json::json!({
            "spec":"event-relay-subscription/1",
            "descriptor":{
                "spec":"event-relay/1",
                "id":"urn:event-relay-channel:01K00000000000000000000000",
                "profiles":[GIT_PROFILE],
                "bindings":[{"type":"nats","endpoint":"tls://events.example.net:4222","subject_filter":"events.channels.channel.>"}],
                "authorization":["bearer_capability"],
                "retention":[{"id":"all","types":["*"],"class":"ephemeral"}],
                "limits":{"event_bytes":65536}
            },
            "credential":{"scheme":"bearer_capability","token":"er1.client.0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"}
        }))
        .expect("bundle");
        let subscription_id = store
            .import(
                &registration,
                source.clone(),
                vec!["refs/heads/main".to_string()],
                bundle,
                false,
            )
            .expect("subscription")
            .subscription
            .id;
        let supervisor =
            Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor"));
        let router = NotificationRouter::new(store, registry, Arc::clone(&supervisor));
        Fixture {
            _temporary: temporary,
            router,
            supervisor,
            subscription_id,
            source,
        }
    }

    fn event(source: &str, id: &str, reference: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "specversion":"1.0",
            "id":id,
            "source":source,
            "type":"dev.tionis.git.ref.state.v1",
            "subject":reference,
            "time":"2026-09-02T20:00:00Z",
            "datacontenttype":"application/json",
            "data":{
                "object_format":"sha1",
                "ref":reference,
                "oid":"89abcdef0123456789abcdef0123456789abcdef"
            }
        }))
        .expect("event")
    }

    #[test]
    fn matching_event_enqueues_one_remote_notification_and_deduplicates() {
        let fixture = fixture();
        let payload = event(&fixture.source, "event-1", "refs/heads/main");
        let first = fixture
            .router
            .route(fixture.subscription_id, &payload)
            .expect("first route");
        assert_eq!(first.disposition, NotificationDisposition::Enqueued);
        let duplicate = fixture
            .router
            .route(fixture.subscription_id, &payload)
            .expect("duplicate route");
        assert_eq!(duplicate.disposition, NotificationDisposition::Duplicate);
        let jobs = fixture.supervisor.list().expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].triggers, vec![SyncJobTrigger::RemoteNotification]);
    }

    #[test]
    fn mismatched_source_ref_and_malformed_events_never_enqueue() {
        let fixture = fixture();
        let wrong_source = fixture
            .router
            .route(
                fixture.subscription_id,
                &event(
                    "urn:git-repository:01K00000000000000000000099",
                    "event-1",
                    "refs/heads/main",
                ),
            )
            .expect("source mismatch");
        assert_eq!(wrong_source.disposition, NotificationDisposition::Rejected);
        let wrong_ref = fixture
            .router
            .route(
                fixture.subscription_id,
                &event(&fixture.source, "event-2", "refs/heads/other"),
            )
            .expect("ref mismatch");
        assert_eq!(wrong_ref.disposition, NotificationDisposition::IgnoredRef);
        let malformed = fixture
            .router
            .route(fixture.subscription_id, b"not JSON")
            .expect("malformed event");
        assert_eq!(malformed.disposition, NotificationDisposition::Rejected);
        assert!(fixture.supervisor.list().expect("jobs").is_empty());
    }
}
