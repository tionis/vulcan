use crate::{
    GitEngine, GitEngineError, GitOid, GitPushResult, GitRefDeleteResult, GitRefName, GitRemote,
    GitRepository,
};
use serde::Deserialize;
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use url::{Host, Url};

pub const NOTIFICATION_ADVERTISEMENT_REF: &str = "refs/vulcan/notifications";
pub const NOTIFICATION_ADVERTISEMENT_FILE: &str = "notification.json";
const NOTIFICATION_ADVERTISEMENT_VERSION: u32 = 1;
const NOTIFICATION_ADVERTISEMENT_TRANSPORT: &str = "http_long_poll";
const NOTIFICATION_ADVERTISEMENT_COMMIT_MESSAGE: &str = "vulcan sync notification advertisement\n";
const MAX_ADVERTISEMENT_BYTES: usize = 16 * 1024;
const MAX_ENDPOINT_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationTransport {
    HttpLongPoll,
}

#[derive(Clone, PartialEq, Eq)]
pub struct NotificationEndpoint {
    url: Url,
    origin: String,
    fingerprint: String,
}

impl NotificationEndpoint {
    fn parse(value: &str) -> Result<Self, NotificationAdvertisementError> {
        if value.len() > MAX_ENDPOINT_BYTES {
            return Err(NotificationAdvertisementError::Invalid(
                "notification subscribe URL exceeds the 4096-byte limit".to_string(),
            ));
        }
        let url = Url::parse(value).map_err(|error| {
            NotificationAdvertisementError::Invalid(format!(
                "notification subscribe URL is invalid: {error}"
            ))
        })?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(NotificationAdvertisementError::Invalid(
                "notification subscribe URL must not contain user information".to_string(),
            ));
        }
        if url.fragment().is_some() {
            return Err(NotificationAdvertisementError::Invalid(
                "notification subscribe URL must not contain a fragment".to_string(),
            ));
        }
        let secure = url.scheme() == "https";
        let loopback_http =
            url.scheme() == "http" && url.host().as_ref().is_some_and(is_loopback_host);
        if !secure && !loopback_http {
            return Err(NotificationAdvertisementError::Invalid(
                "notification subscribe URL must use HTTPS, except for an HTTP loopback endpoint"
                    .to_string(),
            ));
        }
        let origin = url.origin().ascii_serialization();
        let fingerprint = blake3::hash(value.as_bytes()).to_hex()[..16].to_string();
        Ok(Self {
            url,
            origin,
            fingerprint,
        })
    }

    #[must_use]
    pub fn expose_url(&self) -> &Url {
        &self.url
    }

    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

impl Debug for NotificationEndpoint {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NotificationEndpoint")
            .field("origin", &self.origin)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationAdvertisement {
    pub version: u32,
    pub transport: NotificationTransport,
    pub endpoint: NotificationEndpoint,
}

impl NotificationAdvertisement {
    pub fn parse(bytes: &[u8]) -> Result<Self, NotificationAdvertisementError> {
        if bytes.len() > MAX_ADVERTISEMENT_BYTES {
            return Err(NotificationAdvertisementError::Invalid(format!(
                "notification advertisement exceeds the {MAX_ADVERTISEMENT_BYTES}-byte limit"
            )));
        }
        let raw: RawNotificationAdvertisement = serde_json::from_slice(bytes).map_err(|error| {
            NotificationAdvertisementError::Invalid(format!(
                "notification advertisement is not valid JSON: {error}"
            ))
        })?;
        if raw.version != NOTIFICATION_ADVERTISEMENT_VERSION {
            return Err(NotificationAdvertisementError::Invalid(format!(
                "unsupported notification advertisement version `{}`",
                raw.version
            )));
        }
        if raw.transport != NOTIFICATION_ADVERTISEMENT_TRANSPORT {
            return Err(NotificationAdvertisementError::Invalid(
                "unsupported notification transport".to_string(),
            ));
        }
        Ok(Self {
            version: raw.version,
            transport: NotificationTransport::HttpLongPoll,
            endpoint: NotificationEndpoint::parse(&raw.subscribe_url)?,
        })
    }
}

#[derive(Deserialize)]
struct RawNotificationAdvertisement {
    version: u32,
    transport: String,
    subscribe_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredNotificationAdvertisement {
    pub revision: GitOid,
    pub advertisement: NotificationAdvertisement,
}

pub fn refresh_notification_advertisement(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &GitRemote,
) -> Result<Option<DiscoveredNotificationAdvertisement>, NotificationAdvertisementError> {
    let advertisement_ref = GitRefName::parse(NOTIFICATION_ADVERTISEMENT_REF)?;
    let remote_revision = engine.remote_ref(repository, remote, &advertisement_ref)?;
    let local_revision = engine.read_ref(repository, &advertisement_ref)?;
    let Some(remote_revision) = remote_revision else {
        if let Some(local_revision) = local_revision {
            match engine.delete_ref(repository, &advertisement_ref, &local_revision)? {
                GitRefDeleteResult::Deleted | GitRefDeleteResult::Missing => {}
                GitRefDeleteResult::Stale => {
                    return Err(NotificationAdvertisementError::Invalid(
                        "notification advertisement changed while removing a stale local ref"
                            .to_string(),
                    ));
                }
            }
        }
        return Ok(None);
    };
    if local_revision.as_ref() != Some(&remote_revision) {
        let fetched =
            engine.fetch_ref(repository, remote, &advertisement_ref, &advertisement_ref)?;
        if fetched != remote_revision {
            return Err(NotificationAdvertisementError::Invalid(
                "notification advertisement changed while it was being fetched".to_string(),
            ));
        }
    }
    let object = engine
        .path_object(repository, &remote_revision, NOTIFICATION_ADVERTISEMENT_FILE)?
        .ok_or_else(|| {
            NotificationAdvertisementError::Invalid(format!(
                "notification advertisement commit does not contain `{NOTIFICATION_ADVERTISEMENT_FILE}`"
            ))
        })?;
    if object.kind != "blob" || !matches!(object.mode.as_str(), "100644" | "100755") {
        return Err(NotificationAdvertisementError::Invalid(format!(
            "`{NOTIFICATION_ADVERTISEMENT_FILE}` must be a regular file"
        )));
    }
    let bytes = object.data.ok_or_else(|| {
        NotificationAdvertisementError::Invalid(format!(
            "`{NOTIFICATION_ADVERTISEMENT_FILE}` has no readable content"
        ))
    })?;
    Ok(Some(DiscoveredNotificationAdvertisement {
        revision: remote_revision,
        advertisement: NotificationAdvertisement::parse(&bytes)?,
    }))
}

fn is_loopback_host(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
}

/// Publishes a version 1 notification advertisement as a parentless commit
/// carrying only `notification.json`, using pure object-store plumbing
/// (`hash-object`, `mktree`, `commit-tree`) without touching the worktree, the
/// user's index, or any temporary files.
///
/// The candidate URL is validated with the same policy as discovery before any
/// Git object is created, so invalid input never mutates the repository. With
/// `expected: None`, the current remote revision is read first and used as the
/// compare-and-swap lease, so concurrent publishers fail with a stale error
/// instead of silently overwriting each other; only a genuinely absent ref
/// pushes without a lease. Pass an explicit revision for a strict lease.
pub fn publish_notification_advertisement(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &GitRemote,
    subscribe_url: &str,
    expected: Option<&GitOid>,
) -> Result<DiscoveredNotificationAdvertisement, NotificationAdvertisementError> {
    let payload = serde_json::to_vec(&serde_json::json!({
        "version": NOTIFICATION_ADVERTISEMENT_VERSION,
        "transport": NOTIFICATION_ADVERTISEMENT_TRANSPORT,
        "subscribe_url": subscribe_url,
    }))
    .map_err(|error| {
        NotificationAdvertisementError::Invalid(format!(
            "notification advertisement is not valid JSON: {error}"
        ))
    })?;
    let advertisement = NotificationAdvertisement::parse(&payload)?;
    let advertisement_ref = GitRefName::parse(NOTIFICATION_ADVERTISEMENT_REF)?;
    let lease = match expected {
        Some(revision) => Some(revision.clone()),
        None => engine.remote_ref(repository, remote, &advertisement_ref)?,
    };
    let blob = engine.write_blob(repository, &payload)?;
    let tree =
        engine.create_single_file_tree(repository, NOTIFICATION_ADVERTISEMENT_FILE, &blob)?;
    let revision = engine.create_commit(
        repository,
        &tree,
        &[],
        NOTIFICATION_ADVERTISEMENT_COMMIT_MESSAGE,
    )?;
    match engine.push_ref(repository, remote, &revision, &advertisement_ref, lease.as_ref())? {
        GitPushResult::Updated => Ok(DiscoveredNotificationAdvertisement {
            revision,
            advertisement,
        }),
        GitPushResult::Rejected => Err(NotificationAdvertisementError::Invalid(
            "notification advertisement changed while it was being published; read the current revision and retry".to_string(),
        )),
    }
}

/// Deletes the remote notification advertisement with an exact lease. A
/// missing remote ref reports `Missing` and a diverged one reports `Stale`
/// without mutation.
pub fn remove_notification_advertisement(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &GitRemote,
    expected: &GitOid,
) -> Result<GitRefDeleteResult, NotificationAdvertisementError> {
    let advertisement_ref = GitRefName::parse(NOTIFICATION_ADVERTISEMENT_REF)?;
    Ok(engine.delete_remote_ref(repository, remote, &advertisement_ref, expected)?)
}

#[derive(Debug)]
pub enum NotificationAdvertisementError {
    Git(GitEngineError),
    Invalid(String),
}

impl Display for NotificationAdvertisementError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Git(error) => Display::fmt(error, formatter),
            Self::Invalid(detail) => formatter.write_str(detail),
        }
    }
}

impl Error for NotificationAdvertisementError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Git(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<GitEngineError> for NotificationAdvertisementError {
    fn from(error: GitEngineError) -> Self {
        Self::Git(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GitCliEngine;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn parse(value: &str) -> Result<NotificationAdvertisement, NotificationAdvertisementError> {
        NotificationAdvertisement::parse(value.as_bytes())
    }

    #[test]
    fn validates_and_redacts_notification_endpoints() {
        let advertisement = parse(
            r#"{"version":1,"transport":"http_long_poll","subscribe_url":"https://patch.example/h/private?pubsub=true","future":true}"#,
        )
        .expect("valid advertisement");
        assert_eq!(advertisement.version, 1);
        assert_eq!(advertisement.transport, NotificationTransport::HttpLongPoll);
        assert_eq!(advertisement.endpoint.origin(), "https://patch.example");
        assert_eq!(advertisement.endpoint.fingerprint().len(), 16);
        let debug = format!("{advertisement:?}");
        assert!(!debug.contains("private"));
        assert!(!debug.contains("pubsub"));
    }

    #[test]
    fn permits_only_https_and_loopback_http_without_url_authority_credentials() {
        for endpoint in ["http://localhost:3211/wake", "http://127.0.0.1:3211/wake"] {
            parse(&format!(
                r#"{{"version":1,"transport":"http_long_poll","subscribe_url":"{endpoint}"}}"#
            ))
            .expect("loopback HTTP should be valid");
        }
        for endpoint in [
            "http://patch.example/wake",
            "https://user:secret@patch.example/wake",
            "https://patch.example/wake#secret",
            "file:///tmp/wake",
        ] {
            let error = parse(&format!(
                r#"{{"version":1,"transport":"http_long_poll","subscribe_url":"{endpoint}"}}"#
            ))
            .expect_err("unsafe endpoint should be rejected");
            assert!(!error.to_string().contains(endpoint));
        }
    }

    #[test]
    fn rejects_unsupported_and_oversized_advertisements() {
        assert!(parse(
            r#"{"version":2,"transport":"http_long_poll","subscribe_url":"https://patch.example/wake"}"#
        )
        .is_err());
        let transport_error = parse(
            r#"{"version":1,"transport":"secret-transport-token","subscribe_url":"https://patch.example/wake"}"#,
        )
        .expect_err("unsupported transport");
        assert!(!transport_error
            .to_string()
            .contains("secret-transport-token"));
        assert!(
            NotificationAdvertisement::parse(&vec![b' '; MAX_ADVERTISEMENT_BYTES + 1]).is_err()
        );
    }

    #[test]
    fn refreshes_rotated_git_advertisement_and_removes_a_deleted_remote_ref() {
        let temporary = tempdir().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        let repository_path = temporary.path().join("repository");
        run_git(
            temporary.path(),
            &["init", "--bare", remote.to_str().expect("remote")],
        );
        fs::create_dir(&repository_path).expect("repository directory");
        run_git(&repository_path, &["init"]);
        run_git(&repository_path, &["config", "user.name", "Vulcan Tests"]);
        run_git(
            &repository_path,
            &["config", "user.email", "vulcan@example.invalid"],
        );
        run_git(
            &repository_path,
            &["remote", "add", "origin", remote.to_str().expect("remote")],
        );

        write_advertisement(&repository_path, "first");
        run_git(&repository_path, &["add", NOTIFICATION_ADVERTISEMENT_FILE]);
        run_git(
            &repository_path,
            &["commit", "-m", "advertise notifications"],
        );
        run_git(
            &repository_path,
            &["push", "origin", "HEAD:refs/vulcan/notifications"],
        );

        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(&repository_path)
            .expect("discover repository");
        let remote_name = GitRemote::parse("origin").expect("remote name");
        let first = refresh_notification_advertisement(&engine, &repository, &remote_name)
            .expect("discover advertisement")
            .expect("advertisement should exist");
        assert!(first
            .advertisement
            .endpoint
            .expose_url()
            .as_str()
            .contains("first"));

        write_advertisement(&repository_path, "second");
        run_git(&repository_path, &["commit", "-am", "rotate notifications"]);
        run_git(
            &repository_path,
            &[
                "push",
                "--force",
                "origin",
                "HEAD:refs/vulcan/notifications",
            ],
        );
        let second = refresh_notification_advertisement(&engine, &repository, &remote_name)
            .expect("refresh advertisement")
            .expect("rotated advertisement should exist");
        assert_ne!(first.revision, second.revision);
        assert!(second
            .advertisement
            .endpoint
            .expose_url()
            .as_str()
            .contains("second"));

        run_git(
            &repository_path,
            &["push", "origin", ":refs/vulcan/notifications"],
        );
        assert!(
            refresh_notification_advertisement(&engine, &repository, &remote_name)
                .expect("remove stale advertisement")
                .is_none()
        );
        let reference = GitRefName::parse(NOTIFICATION_ADVERTISEMENT_REF).expect("ref");
        assert_eq!(
            engine.read_ref(&repository, &reference).expect("read ref"),
            None
        );
    }

    fn write_advertisement(repository: &Path, channel: &str) {
        fs::write(
            repository.join(NOTIFICATION_ADVERTISEMENT_FILE),
            format!(
                r#"{{"version":1,"transport":"http_long_poll","subscribe_url":"https://patch.example/h/{channel}?pubsub=true"}}"#
            ),
        )
        .expect("write advertisement");
    }

    fn init_publish_fixture() -> (tempfile::TempDir, GitCliEngine, GitRepository, GitRemote) {
        let temporary = tempdir().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        let repository_path = temporary.path().join("repository");
        run_git(
            temporary.path(),
            &["init", "--bare", remote.to_str().expect("remote")],
        );
        fs::create_dir(&repository_path).expect("repository directory");
        run_git(&repository_path, &["init"]);
        run_git(&repository_path, &["config", "user.name", "Vulcan Tests"]);
        run_git(
            &repository_path,
            &["config", "user.email", "vulcan@example.invalid"],
        );
        run_git(
            &repository_path,
            &["remote", "add", "origin", remote.to_str().expect("remote")],
        );
        let engine = GitCliEngine::default();
        let repository = engine
            .discover_repository(&repository_path)
            .expect("discover repository");
        let remote_name = GitRemote::parse("origin").expect("remote name");
        (temporary, engine, repository, remote_name)
    }

    fn publish(
        engine: &GitCliEngine,
        repository: &GitRepository,
        remote: &GitRemote,
        channel: &str,
        expected: Option<&GitOid>,
    ) -> DiscoveredNotificationAdvertisement {
        publish_notification_advertisement(
            engine,
            repository,
            remote,
            &format!("https://patch.example/h/{channel}?pubsub=true"),
            expected,
        )
        .expect("publish advertisement")
    }

    #[test]
    fn publishes_parentless_advertisement_without_touching_the_worktree() {
        let (_temporary, engine, repository, remote) = init_publish_fixture();
        let published = publish(&engine, &repository, &remote, "plumbing", None);
        assert_eq!(published.advertisement.version, 1);
        assert_eq!(
            published.advertisement.transport,
            NotificationTransport::HttpLongPoll
        );
        assert!(published
            .advertisement
            .endpoint
            .expose_url()
            .as_str()
            .contains("plumbing"));
        assert!(!format!("{published:?}").contains("plumbing"));

        let metadata = engine
            .commit_metadata(&repository, &published.revision)
            .expect("commit metadata");
        assert!(metadata.parents.is_empty());
        assert!(!metadata.message.contains("plumbing"));

        let entries = engine
            .tree_entries(&repository, &published.revision)
            .expect("tree entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, NOTIFICATION_ADVERTISEMENT_FILE);
        assert_eq!(entries[0].mode, "100644");
        assert_eq!(entries[0].kind, "blob");

        let work_tree = repository
            .work_tree
            .as_ref()
            .expect("publishing repository has a worktree");
        let leftovers = fs::read_dir(work_tree)
            .expect("read worktree")
            .map(|entry| {
                entry
                    .expect("worktree entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(leftovers, [".git".to_string()]);

        let refreshed = refresh_notification_advertisement(&engine, &repository, &remote)
            .expect("refresh advertisement")
            .expect("advertisement should exist");
        assert_eq!(refreshed.revision, published.revision);
        assert_eq!(refreshed.advertisement, published.advertisement);
    }

    #[test]
    fn rotates_with_exact_lease_and_rejects_stale_leases() {
        let (_temporary, engine, repository, remote) = init_publish_fixture();
        let first = publish(&engine, &repository, &remote, "first", None);
        let second = publish(
            &engine,
            &repository,
            &remote,
            "second",
            Some(&first.revision),
        );
        assert_ne!(first.revision, second.revision);
        assert!(second
            .advertisement
            .endpoint
            .expose_url()
            .as_str()
            .contains("second"));

        let stale = publish_notification_advertisement(
            &engine,
            &repository,
            &remote,
            "https://patch.example/h/stale?pubsub=true",
            Some(&first.revision),
        )
        .expect_err("stale lease should be rejected");
        assert!(stale.to_string().contains("changed"));
        assert!(!stale.to_string().contains("stale"));

        let opportunistic = publish(&engine, &repository, &remote, "third", None);
        assert_ne!(opportunistic.revision, second.revision);
        let refreshed = refresh_notification_advertisement(&engine, &repository, &remote)
            .expect("refresh advertisement")
            .expect("advertisement should exist");
        assert_eq!(refreshed.revision, opportunistic.revision);
    }

    #[test]
    fn rejects_invalid_subscribe_url_before_git_mutation() {
        let (_temporary, engine, repository, remote) = init_publish_fixture();
        let error = publish_notification_advertisement(
            &engine,
            &repository,
            &remote,
            "http://patch.example/h/insecure?pubsub=true",
            None,
        )
        .expect_err("insecure endpoint should be rejected");
        assert!(!error.to_string().contains("insecure"));
        assert_eq!(
            engine
                .remote_ref(
                    &repository,
                    &remote,
                    &GitRefName::parse(NOTIFICATION_ADVERTISEMENT_REF).expect("ref")
                )
                .expect("read remote ref"),
            None
        );
    }

    #[test]
    fn removes_advertisement_with_exact_lease() {
        let (_temporary, engine, repository, remote) = init_publish_fixture();
        let published = publish(&engine, &repository, &remote, "ephemeral", None);
        let other = GitOid::parse("0".repeat(40)).expect("other revision");
        assert_ne!(other, published.revision);
        assert_eq!(
            remove_notification_advertisement(&engine, &repository, &remote, &other)
                .expect("stale removal"),
            GitRefDeleteResult::Stale
        );
        assert!(
            refresh_notification_advertisement(&engine, &repository, &remote)
                .expect("refresh advertisement")
                .is_some()
        );
        assert_eq!(
            remove_notification_advertisement(&engine, &repository, &remote, &published.revision)
                .expect("leased removal"),
            GitRefDeleteResult::Deleted
        );
        assert!(
            refresh_notification_advertisement(&engine, &repository, &remote)
                .expect("refresh after removal")
                .is_none()
        );
    }

    fn run_git(directory: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
