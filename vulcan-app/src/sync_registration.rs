//! Per-device registration records in a vault's Git remote (Roadmap 12.21.2).
//!
//! Each device owns `refs/heads/__vulcan-sync/registrations/<device-id>`, a
//! parentless-or-linear history whose tree holds one strict `registration.json`.
//! The list is trusted as written: anyone with push access can change it. See
//! `docs/specs/device-transport-auth.md` for that accepted tradeoff.

use crate::device_identity::{identity_from_public_key, DeviceIdentityStore};
use crate::sync_devices::{is_remote_observation_unavailable, validate_device_name};
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use vulcan_core::VaultPaths;
use vulcan_sync::{
    remote_registration_ref, GitEngine, GitOid, GitPushResult, GitRefDeleteResult, GitRefName,
    GitReference, GitRemote, GitRepository, GitSyncDeviceId, GitSyncDeviceIdKind, RepositoryLock,
    LOCAL_REGISTRATION_MIRROR_ROOT, REMOTE_REGISTRATION_BRANCH_ROOT,
};

pub const SYNC_REGISTRATION_REPORT_VERSION: u32 = 1;
const REGISTRATION_VERSION: u32 = 1;
const REGISTRATION_FILE: &str = "registration.json";
const MAX_REGISTRATION_BYTES: usize = 4096;
const LOCAL_MIRROR_ROOT: &str = LOCAL_REGISTRATION_MIRROR_ROOT;
const COMMIT_MESSAGE: &str = "vulcan device registration\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationStatus {
    /// Created by an administrator for a device that has not synced yet.
    Placeholder,
    /// Written by the device itself.
    Registered,
    /// Tombstone: honest devices never overwrite it.
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRegistration {
    pub version: u32,
    pub device_id: String,
    /// Canonical `ssh-ed25519 <base64>` without a comment.
    pub public_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub status: RegistrationStatus,
    pub created_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at_unix: Option<u64>,
}

impl DeviceRegistration {
    /// Strictly parses one stored record. Never trusts a claimed ID: it must
    /// derive from the key.
    pub fn parse(bytes: &[u8]) -> Result<Self, AppError> {
        if bytes.len() > MAX_REGISTRATION_BYTES {
            return Err(AppError::operation("registration exceeds its size limit"));
        }
        let record: Self = serde_json::from_slice(bytes)
            .map_err(|error| AppError::operation(format!("invalid registration: {error}")))?;
        if record.version != REGISTRATION_VERSION {
            return Err(AppError::operation(format!(
                "unsupported registration version {}",
                record.version
            )));
        }
        let identity = identity_from_public_key(&record.public_key, false)?;
        if identity.device_id != record.device_id {
            return Err(AppError::operation(
                "registration device ID does not match its public key",
            ));
        }
        if let Some(label) = &record.label {
            validate_device_name(label)?;
        }
        match (record.status, record.claimed_at_unix) {
            (RegistrationStatus::Placeholder, Some(_)) => {
                return Err(AppError::operation("a placeholder cannot be claimed"))
            }
            (RegistrationStatus::Registered, None) => {
                return Err(AppError::operation(
                    "a registered device must record when it claimed its registration",
                ))
            }
            _ => {}
        }
        Ok(record)
    }

    fn fingerprint(&self) -> Result<String, AppError> {
        Ok(identity_from_public_key(&self.public_key, false)?.fingerprint)
    }
}

/// Who is writing a registration change. Writers enforce the transition rules;
/// readers cannot know who pushed a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Actor {
    Device,
    Admin,
}

fn check_transition(
    old: Option<&DeviceRegistration>,
    new: &DeviceRegistration,
    actor: Actor,
) -> Result<(), AppError> {
    use RegistrationStatus::{Placeholder, Registered, Revoked};
    if let Some(old) = old {
        if old.device_id != new.device_id
            || old.public_key != new.public_key
            || old.created_at_unix != new.created_at_unix
        {
            return Err(AppError::operation(
                "a registration's identity, key, and creation time never change",
            ));
        }
    }
    let allowed = matches!(
        (old.map(|record| record.status), new.status, actor),
        (None | Some(Placeholder), Placeholder, Actor::Admin)
            | (
                None | Some(Placeholder | Registered),
                Registered,
                Actor::Device
            )
            | (Some(Placeholder | Registered), Revoked, Actor::Admin)
            | (Some(Revoked), Revoked, _)
    );
    if allowed {
        Ok(())
    } else {
        Err(AppError::operation(format!(
            "illegal registration change from {:?} to {:?} by the {}",
            old.map(|record| record.status),
            new.status,
            if actor == Actor::Device {
                "device"
            } else {
                "administrator"
            }
        )))
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn mirror_ref(device_id: &str) -> Result<GitRefName, AppError> {
    GitRefName::parse(format!("{LOCAL_MIRROR_ROOT}/{device_id}")).map_err(AppError::operation)
}

fn parse_device_id(text: &str) -> Result<GitSyncDeviceId, AppError> {
    let id = GitSyncDeviceId::parse(text).map_err(AppError::operation)?;
    if id.kind() != GitSyncDeviceIdKind::SshKeyV1 {
        return Err(AppError::operation(
            "registrations use key-derived `vdev1_` device IDs; pass the full ID",
        ));
    }
    Ok(id)
}

struct Context<'a> {
    engine: &'a dyn GitEngine,
    repository: &'a GitRepository,
    remote: &'a GitRemote,
}

impl Context<'_> {
    /// Reads and validates the record committed at `revision`. Failures are
    /// reasons, not errors, because remote content is untrusted.
    fn read_record(
        &self,
        revision: &GitOid,
        expected_id: &str,
    ) -> Result<DeviceRegistration, String> {
        let entries = self
            .engine
            .tree_entries(self.repository, revision)
            .map_err(|error| error.to_string())?;
        let [entry] = entries.as_slice() else {
            return Err("registration tree must contain exactly one file".to_owned());
        };
        if entry.path != REGISTRATION_FILE
            || entry.kind != "blob"
            || !matches!(entry.mode.as_str(), "100644" | "100755")
        {
            return Err(format!(
                "registration tree must be a regular `{REGISTRATION_FILE}`"
            ));
        }
        let object = self
            .engine
            .path_object(self.repository, revision, REGISTRATION_FILE)
            .map_err(|error| error.to_string())?
            .and_then(|object| object.data)
            .ok_or_else(|| "registration file is unreadable".to_owned())?;
        let record = DeviceRegistration::parse(&object).map_err(|error| error.to_string())?;
        if record.device_id != expected_id {
            return Err("registration ref names a different device".to_owned());
        }
        Ok(record)
    }

    /// Fetches the device's remote record into the local mirror and reads it.
    fn fetch_record(
        &self,
        device_id: &str,
    ) -> Result<Option<(DeviceRegistration, GitOid)>, AppError> {
        let remote_ref = remote_registration_ref(device_id).map_err(AppError::operation)?;
        let Some(revision) = self
            .engine
            .remote_ref(self.repository, self.remote, &remote_ref)
            .map_err(AppError::operation)?
        else {
            return Ok(None);
        };
        self.mirror(&remote_ref, device_id, &revision)?;
        self.read_record(&revision, device_id)
            .map(|record| Some((record, revision)))
            .map_err(|reason| AppError::operation(format!("rejected registration: {reason}")))
    }

    fn mirror(
        &self,
        remote_ref: &GitRefName,
        device_id: &str,
        revision: &GitOid,
    ) -> Result<(), AppError> {
        let local = mirror_ref(device_id)?;
        let fetched = self
            .engine
            .fetch_ref(self.repository, self.remote, remote_ref, &local)
            .map_err(AppError::operation)?;
        if &fetched == revision {
            Ok(())
        } else {
            Err(AppError::operation(
                "registration changed while it was being fetched; retry",
            ))
        }
    }

    /// Publishes `record` as the next commit. `parent` is both the commit
    /// parent and the exact lease; `None` is create-only.
    fn publish(
        &self,
        record: &DeviceRegistration,
        parent: Option<&GitOid>,
    ) -> Result<GitOid, AppError> {
        let mut bytes = serde_json::to_vec_pretty(record).map_err(AppError::operation)?;
        bytes.push(b'\n');
        let blob = self
            .engine
            .write_blob(self.repository, &bytes)
            .map_err(AppError::operation)?;
        let tree = self
            .engine
            .create_single_file_tree(self.repository, REGISTRATION_FILE, &blob)
            .map_err(AppError::operation)?;
        let parents = parent.cloned().into_iter().collect::<Vec<_>>();
        let commit = self
            .engine
            .create_commit(self.repository, &tree, &parents, COMMIT_MESSAGE)
            .map_err(AppError::operation)?;
        let remote_ref = remote_registration_ref(&record.device_id).map_err(AppError::operation)?;
        match self
            .engine
            .push_ref(self.repository, self.remote, &commit, &remote_ref, parent)
            .map_err(AppError::operation)?
        {
            GitPushResult::Updated => {
                self.mirror(&remote_ref, &record.device_id, &commit)?;
                Ok(commit)
            }
            GitPushResult::Rejected => Err(AppError::operation(
                "registration changed concurrently; re-run to read the current state",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationAction {
    Created,
    Updated,
    Unchanged,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistrationChangeReport {
    pub version: u32,
    pub remote: GitRemote,
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<RegistrationStatus>,
    pub action: RegistrationAction,
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

fn open(paths: &VaultPaths) -> Result<(vulcan_sync::GitCliEngine, GitRepository), AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = crate::sync_transport::git_engine(paths);
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    Ok((engine, repository))
}

/// Creates a key-bearing placeholder for a device that has not synced yet.
pub fn register_placeholder(
    paths: &VaultPaths,
    remote: &GitRemote,
    public_key: &str,
    label: Option<&str>,
    dry_run: bool,
) -> Result<RegistrationChangeReport, AppError> {
    let identity = identity_from_public_key(public_key, true)?;
    if let Some(label) = label {
        validate_device_name(label)?;
    }
    let (engine, repository) = open(paths)?;
    let _lock = RepositoryLock::acquire(&repository.git_dir)?;
    let context = Context {
        engine: &engine,
        repository: &repository,
        remote,
    };
    let existing = context.fetch_record(&identity.device_id)?;
    let candidate = DeviceRegistration {
        version: REGISTRATION_VERSION,
        device_id: identity.device_id.clone(),
        public_key: identity.public_key,
        label: label.map(str::to_owned),
        status: RegistrationStatus::Placeholder,
        created_at_unix: existing
            .as_ref()
            .map_or_else(now_unix, |(record, _)| record.created_at_unix),
        claimed_at_unix: None,
    };
    let (action, parent) = match &existing {
        None => (RegistrationAction::Created, None),
        Some((record, _)) if record.status == RegistrationStatus::Registered => {
            // The device already claimed its slot; nothing for an admin to add.
            return change_report(remote, record, RegistrationAction::Unchanged, dry_run, None);
        }
        Some((record, _)) if record.status == RegistrationStatus::Revoked => {
            return Err(AppError::operation(
                "this device is revoked; run `vulcan sync devices unregister` first to start over",
            ))
        }
        Some((record, _)) if record.label == candidate.label => {
            return change_report(remote, record, RegistrationAction::Unchanged, dry_run, None)
        }
        Some((_, revision)) => (RegistrationAction::Updated, Some(revision.clone())),
    };
    check_transition(
        existing.as_ref().map(|(record, _)| record),
        &candidate,
        Actor::Admin,
    )?;
    let revision = if dry_run {
        None
    } else {
        Some(context.publish(&candidate, parent.as_ref())?.to_string())
    };
    change_report(remote, &candidate, action, dry_run, revision)
}

/// Marks a registration revoked, keeping the tombstone.
pub fn revoke_registration(
    paths: &VaultPaths,
    remote: &GitRemote,
    device_id: &str,
    dry_run: bool,
) -> Result<RegistrationChangeReport, AppError> {
    let device_id = parse_device_id(device_id)?;
    let (engine, repository) = open(paths)?;
    let _lock = RepositoryLock::acquire(&repository.git_dir)?;
    let context = Context {
        engine: &engine,
        repository: &repository,
        remote,
    };
    let Some((record, revision)) = context.fetch_record(device_id.as_str())? else {
        return Err(AppError::operation(
            "no registration for that device; register its public key first, or remove its key at the forge",
        ));
    };
    if record.status == RegistrationStatus::Revoked {
        return change_report(
            remote,
            &record,
            RegistrationAction::Unchanged,
            dry_run,
            None,
        );
    }
    let revoked = DeviceRegistration {
        status: RegistrationStatus::Revoked,
        ..record.clone()
    };
    check_transition(Some(&record), &revoked, Actor::Admin)?;
    let published = if dry_run {
        None
    } else {
        Some(context.publish(&revoked, Some(&revision))?.to_string())
    };
    change_report(
        remote,
        &revoked,
        RegistrationAction::Updated,
        dry_run,
        published,
    )
}

/// Deletes a registration entirely. Removes no forge key.
pub fn unregister_registration(
    paths: &VaultPaths,
    remote: &GitRemote,
    device_id: &str,
    dry_run: bool,
) -> Result<RegistrationChangeReport, AppError> {
    let device_id = parse_device_id(device_id)?;
    let (engine, repository) = open(paths)?;
    let _lock = RepositoryLock::acquire(&repository.git_dir)?;
    let remote_ref = remote_registration_ref(device_id.as_str()).map_err(AppError::operation)?;
    let revision = engine
        .remote_ref(&repository, remote, &remote_ref)
        .map_err(AppError::operation)?;
    let mut report = RegistrationChangeReport {
        version: SYNC_REGISTRATION_REPORT_VERSION,
        remote: remote.clone(),
        device_id: device_id.as_str().to_owned(),
        fingerprint: None,
        status: None,
        action: RegistrationAction::Unchanged,
        dry_run,
        revision: None,
    };
    let Some(revision) = revision else {
        return Ok(report);
    };
    report.action = RegistrationAction::Removed;
    report.revision = Some(revision.to_string());
    if dry_run {
        return Ok(report);
    }
    match engine
        .delete_remote_ref(&repository, remote, &remote_ref, &revision)
        .map_err(AppError::operation)?
    {
        GitRefDeleteResult::Deleted => {}
        GitRefDeleteResult::Missing => report.action = RegistrationAction::Unchanged,
        GitRefDeleteResult::Stale => {
            return Err(AppError::operation(
                "registration changed concurrently; re-run to read the current state",
            ))
        }
    }
    let mirror = mirror_ref(device_id.as_str())?;
    if let Some(local) = engine
        .read_ref(&repository, &mirror)
        .map_err(AppError::operation)?
    {
        engine
            .delete_ref(&repository, &mirror, &local)
            .map_err(AppError::operation)?;
    }
    Ok(report)
}

fn change_report(
    remote: &GitRemote,
    record: &DeviceRegistration,
    action: RegistrationAction,
    dry_run: bool,
    revision: Option<String>,
) -> Result<RegistrationChangeReport, AppError> {
    Ok(RegistrationChangeReport {
        version: SYNC_REGISTRATION_REPORT_VERSION,
        remote: remote.clone(),
        device_id: record.device_id.clone(),
        fingerprint: Some(record.fingerprint()?),
        status: Some(record.status),
        action,
        dry_run,
        revision,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationSource {
    Remote,
    /// Last fetched copy; the remote was not asked.
    LocalMirror,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationObservation {
    Observed,
    NotRequested,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistrationSummary {
    pub device_id: String,
    pub fingerprint: String,
    pub public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub status: RegistrationStatus,
    pub created_at_unix: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_at_unix: Option<u64>,
    pub current_device: bool,
    pub revision: String,
    pub source: RegistrationSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectedRegistration {
    pub reference: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistrationListReport {
    pub version: u32,
    pub remote: GitRemote,
    pub observation: RegistrationObservation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_device_id: Option<String>,
    pub count: usize,
    pub registrations: Vec<RegistrationSummary>,
    /// Malformed or inconsistent records; never fatal and never trusted.
    pub rejected: Vec<RejectedRegistration>,
}

/// Lists registrations. With `observe_remote` false, only the last fetched
/// copies are read and no remote is contacted.
pub fn list_registrations(
    paths: &VaultPaths,
    remote: &GitRemote,
    observe_remote: bool,
) -> Result<RegistrationListReport, AppError> {
    list_registrations_with_current(
        paths,
        remote,
        observe_remote,
        DeviceIdentityStore::user_default()
            .ok()
            .and_then(|store| store.device_id().ok().flatten())
            .as_deref(),
    )
}

type Candidate = (String, GitRefName, GitOid, RegistrationSource);

/// Remote registration refs when observable, otherwise the last fetched mirror.
fn collect_candidates(
    context: &Context<'_>,
    observe_remote: bool,
) -> Result<(RegistrationObservation, Vec<Candidate>), AppError> {
    let mut observation = RegistrationObservation::NotRequested;
    let mut candidates: Vec<Candidate> = Vec::new();
    let remote_prefix =
        GitRefName::parse(REMOTE_REGISTRATION_BRANCH_ROOT).map_err(AppError::operation)?;
    if observe_remote {
        match context
            .engine
            .list_remote_refs(context.repository, context.remote, &remote_prefix)
        {
            Ok(references) => {
                observation = RegistrationObservation::Observed;
                for reference in references {
                    let id = reference
                        .name
                        .as_str()
                        .strip_prefix(&format!("{REMOTE_REGISTRATION_BRANCH_ROOT}/"))
                        .unwrap_or_default()
                        .to_owned();
                    candidates.push((
                        id,
                        reference.name,
                        reference.target,
                        RegistrationSource::Remote,
                    ));
                }
            }
            Err(error) if is_remote_observation_unavailable(&error) => {
                observation = RegistrationObservation::Unavailable;
            }
            Err(error) => return Err(AppError::operation(error)),
        }
    }
    if observation != RegistrationObservation::Observed {
        let mirror_prefix = GitRefName::parse(LOCAL_MIRROR_ROOT).map_err(AppError::operation)?;
        for reference in context
            .engine
            .list_refs(context.repository, &mirror_prefix)
            .map_err(AppError::operation)?
        {
            let id = reference
                .name
                .as_str()
                .strip_prefix(&format!("{LOCAL_MIRROR_ROOT}/"))
                .unwrap_or_default()
                .to_owned();
            candidates.push((
                id,
                reference.name,
                reference.target,
                RegistrationSource::LocalMirror,
            ));
        }
    }
    Ok((observation, candidates))
}

fn list_registrations_with_current(
    paths: &VaultPaths,
    remote: &GitRemote,
    observe_remote: bool,
    current: Option<&str>,
) -> Result<RegistrationListReport, AppError> {
    let (engine, repository) = open(paths)?;
    let _lock = RepositoryLock::acquire(&repository.git_dir)?;
    let context = Context {
        engine: &engine,
        repository: &repository,
        remote,
    };
    let mut report = RegistrationListReport {
        version: SYNC_REGISTRATION_REPORT_VERSION,
        remote: remote.clone(),
        observation: RegistrationObservation::NotRequested,
        current_device_id: current.map(str::to_owned),
        count: 0,
        registrations: Vec::new(),
        rejected: Vec::new(),
    };
    let (observation, candidates) = collect_candidates(&context, observe_remote)?;
    report.observation = observation;
    for (id, reference, revision, source) in candidates {
        let rejected = |reason: String| RejectedRegistration {
            reference: reference.as_str().to_owned(),
            reason,
        };
        if parse_device_id(&id).is_err() {
            report.rejected.push(rejected(
                "ref does not name a key-derived device ID".to_owned(),
            ));
            continue;
        }
        if source == RegistrationSource::Remote {
            if let Err(error) = context.mirror(&reference, &id, &revision) {
                report.rejected.push(rejected(error.to_string()));
                continue;
            }
        }
        match context.read_record(&revision, &id) {
            Ok(record) => {
                let fingerprint = record.fingerprint()?;
                report.registrations.push(RegistrationSummary {
                    current_device: current == Some(record.device_id.as_str()),
                    device_id: record.device_id,
                    fingerprint,
                    public_key: record.public_key,
                    label: record.label,
                    status: record.status,
                    created_at_unix: record.created_at_unix,
                    claimed_at_unix: record.claimed_at_unix,
                    revision: revision.to_string(),
                    source,
                });
            }
            Err(reason) => report.rejected.push(rejected(reason)),
        }
    }
    report
        .registrations
        .sort_by(|a, b| a.device_id.cmp(&b.device_id));
    report.count = report.registrations.len();
    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfRegistrationOutcome {
    Created,
    Claimed,
    AlreadyRegistered,
    /// An administrator revoked this device. It reports this and never
    /// overwrites the tombstone.
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelfRegistrationReport {
    pub outcome: Option<SelfRegistrationOutcome>,
    /// Best-effort failure detail; never fails the sync.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Publishes or claims this device's own registration after a successful,
/// non-dry-run sync. Never fails the sync: errors are returned in the report.
/// Reports `None` when nothing worth surfacing happened (already registered).
///
/// `observed` carries the registration refs the sync saw in its own remote
/// trip, so a steady-state sync adds no network round trip here: the device
/// only fetches when its own record changed (for example a revocation) and
/// only pushes to create or claim it.
pub fn self_register_after_sync(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &GitRemote,
    store: &DeviceIdentityStore,
    observed: Option<&[GitReference]>,
) -> Option<SelfRegistrationReport> {
    match self_register_with_store(engine, repository, remote, store, observed, now_unix()) {
        Ok(Some(
            outcome @ (SelfRegistrationOutcome::Created
            | SelfRegistrationOutcome::Claimed
            | SelfRegistrationOutcome::Revoked),
        )) => Some(SelfRegistrationReport {
            outcome: Some(outcome),
            error: None,
        }),
        Ok(_) => None,
        Err(error) => Some(SelfRegistrationReport {
            outcome: None,
            error: Some(error.to_string()),
        }),
    }
}

impl Context<'_> {
    /// This device's record as of the remote tip `tip`, fetching only when the
    /// local mirror does not already hold that exact revision.
    fn record_at_tip(
        &self,
        device_id: &str,
        tip: &GitOid,
    ) -> Result<(DeviceRegistration, GitOid), AppError> {
        let mirror = mirror_ref(device_id)?;
        let current = self
            .engine
            .read_ref(self.repository, &mirror)
            .map_err(AppError::operation)?;
        if current.as_ref() != Some(tip) {
            let remote_ref = remote_registration_ref(device_id).map_err(AppError::operation)?;
            self.mirror(&remote_ref, device_id, tip)?;
        }
        self.read_record(tip, device_id)
            .map(|record| (record, tip.clone()))
            .map_err(|reason| AppError::operation(format!("rejected registration: {reason}")))
    }

    /// Resolves this device's record without a dedicated remote query when the
    /// sync already observed the namespace.
    fn own_record(
        &self,
        device_id: &str,
        observed: Option<&[GitReference]>,
    ) -> Result<Option<(DeviceRegistration, GitOid)>, AppError> {
        let remote_ref = remote_registration_ref(device_id).map_err(AppError::operation)?;
        if let Some(references) = observed {
            return references
                .iter()
                .find(|reference| reference.name == remote_ref)
                .map(|reference| self.record_at_tip(device_id, &reference.target))
                .transpose();
        }
        // The sync did not observe the namespace. Prefer the last mirrored
        // copy over a new round trip; only a device that has never seen its
        // own record pays for an explicit lookup.
        if let Some(tip) = self
            .engine
            .read_ref(self.repository, &mirror_ref(device_id)?)
            .map_err(AppError::operation)?
        {
            return self.record_at_tip(device_id, &tip).map(Some);
        }
        self.fetch_record(device_id)
    }
}

fn self_register_with_store(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &GitRemote,
    store: &DeviceIdentityStore,
    observed: Option<&[GitReference]>,
    now: u64,
) -> Result<Option<SelfRegistrationOutcome>, AppError> {
    let Some(device_id) = store.device_id()? else {
        return Ok(None);
    };
    let public_key = store.public_key()?;
    let context = Context {
        engine,
        repository,
        remote,
    };
    let _lock = RepositoryLock::acquire(&repository.git_dir)?;
    match context.own_record(&device_id, observed)? {
        Some((record, _)) if record.status == RegistrationStatus::Revoked => {
            Ok(Some(SelfRegistrationOutcome::Revoked))
        }
        Some((record, _)) if record.status == RegistrationStatus::Registered => {
            if record.public_key != public_key {
                return Err(AppError::operation(
                    "registration key does not match this device",
                ));
            }
            Ok(Some(SelfRegistrationOutcome::AlreadyRegistered))
        }
        Some((record, revision)) => {
            let claimed = DeviceRegistration {
                status: RegistrationStatus::Registered,
                claimed_at_unix: Some(now),
                ..record.clone()
            };
            check_transition(Some(&record), &claimed, Actor::Device)?;
            context.publish(&claimed, Some(&revision))?;
            Ok(Some(SelfRegistrationOutcome::Claimed))
        }
        None => {
            let created = DeviceRegistration {
                version: REGISTRATION_VERSION,
                device_id,
                public_key,
                label: None,
                status: RegistrationStatus::Registered,
                created_at_unix: now,
                claimed_at_unix: Some(now),
            };
            check_transition(None, &created, Actor::Device)?;
            context.publish(&created, None)?;
            Ok(Some(SelfRegistrationOutcome::Created))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(path)
            .args(args)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    struct Fixture {
        _dir: TempDir,
        paths: VaultPaths,
        remote: GitRemote,
        remote_path: std::path::PathBuf,
        admin_key: String,
        device: DeviceIdentityStore,
        device_id: String,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().expect("tempdir");
        let remote_path = dir.path().join("remote.git");
        let vault = dir.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        git(dir.path(), &["init", "--bare", "-q", "remote.git"]);
        git(&vault, &["init", "-q"]);
        git(
            &vault,
            &["remote", "add", "origin", remote_path.to_str().unwrap()],
        );
        let other = DeviceIdentityStore::at(dir.path().join("other"));
        other.initialize(false).unwrap();
        let device = DeviceIdentityStore::at(dir.path().join("device"));
        device.initialize(false).unwrap();
        let device_id = device.device_id().unwrap().unwrap();
        Fixture {
            paths: VaultPaths::new(&vault),
            remote: GitRemote::parse("origin").unwrap(),
            remote_path,
            admin_key: other.public_key().unwrap(),
            device,
            device_id,
            _dir: dir,
        }
    }

    impl Fixture {
        fn engine_and_repo(&self) -> (vulcan_sync::GitCliEngine, GitRepository) {
            open(&self.paths).unwrap()
        }

        /// The registration refs the sync's own remote trip would have seen.
        fn observed(&self) -> Vec<GitReference> {
            let (engine, repository) = self.engine_and_repo();
            engine
                .list_remote_refs(
                    &repository,
                    &self.remote,
                    &GitRefName::parse(REMOTE_REGISTRATION_BRANCH_ROOT).unwrap(),
                )
                .unwrap()
        }

        fn self_register(&self) -> Result<Option<SelfRegistrationOutcome>, AppError> {
            let (engine, repository) = self.engine_and_repo();
            let observed = self.observed();
            self_register_with_store(
                &engine,
                &repository,
                &self.remote,
                &self.device,
                Some(&observed),
                10,
            )
        }

        fn self_register_unobserved(&self) -> Result<Option<SelfRegistrationOutcome>, AppError> {
            let (engine, repository) = self.engine_and_repo();
            self_register_with_store(&engine, &repository, &self.remote, &self.device, None, 10)
        }

        fn drop_mirror(&self) {
            git(
                self.paths.vault_root(),
                &[
                    "update-ref",
                    "-d",
                    &format!("{LOCAL_REGISTRATION_MIRROR_ROOT}/{}", self.device_id),
                ],
            );
        }

        fn list(&self) -> RegistrationListReport {
            list_registrations_with_current(&self.paths, &self.remote, true, Some(&self.device_id))
                .unwrap()
        }

        fn device_key(&self) -> String {
            self.device.public_key().unwrap()
        }
    }

    fn sample_record() -> DeviceRegistration {
        let fx = fixture();
        DeviceRegistration {
            version: 1,
            device_id: fx.device_id.clone(),
            public_key: fx.device_key(),
            label: None,
            status: RegistrationStatus::Registered,
            created_at_unix: 10,
            claimed_at_unix: Some(10),
        }
    }

    #[test]
    fn parser_rejects_mismatched_unknown_oversize_and_inconsistent_records() {
        let good = sample_record();
        let bytes = serde_json::to_vec(&good).unwrap();
        assert_eq!(DeviceRegistration::parse(&bytes).unwrap(), good);

        let other = fixture().admin_key;
        let mismatch = DeviceRegistration {
            public_key: other,
            ..good.clone()
        };
        assert!(
            DeviceRegistration::parse(&serde_json::to_vec(&mismatch).unwrap())
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );

        let mut unknown = serde_json::to_value(&good).unwrap();
        unknown["extra"] = true.into();
        assert!(DeviceRegistration::parse(&serde_json::to_vec(&unknown).unwrap()).is_err());

        let commented = DeviceRegistration {
            public_key: format!("{} someone@host", good.public_key),
            ..good.clone()
        };
        assert!(DeviceRegistration::parse(&serde_json::to_vec(&commented).unwrap()).is_err());

        let claimed_placeholder = DeviceRegistration {
            status: RegistrationStatus::Placeholder,
            ..good.clone()
        };
        assert!(
            DeviceRegistration::parse(&serde_json::to_vec(&claimed_placeholder).unwrap()).is_err()
        );
        let unclaimed = DeviceRegistration {
            claimed_at_unix: None,
            ..good.clone()
        };
        assert!(DeviceRegistration::parse(&serde_json::to_vec(&unclaimed).unwrap()).is_err());

        let bad_label = DeviceRegistration {
            label: Some("evil\u{202e}name".to_owned()),
            ..good.clone()
        };
        assert!(DeviceRegistration::parse(&serde_json::to_vec(&bad_label).unwrap()).is_err());

        let oversize = DeviceRegistration {
            label: Some("x".repeat(5000)),
            ..good
        };
        assert!(DeviceRegistration::parse(&serde_json::to_vec(&oversize).unwrap()).is_err());
        assert!(DeviceRegistration::parse(b"{}").is_err());
    }

    #[test]
    fn transitions_are_actor_scoped_and_revoked_is_sticky() {
        use RegistrationStatus::{Placeholder, Registered, Revoked};
        let base = sample_record();
        let with = |status, claimed| DeviceRegistration {
            status,
            claimed_at_unix: claimed,
            ..base.clone()
        };
        let placeholder = with(Placeholder, None);
        let registered = with(Registered, Some(11));
        let revoked = with(Revoked, None);

        assert!(check_transition(None, &placeholder, Actor::Admin).is_ok());
        assert!(check_transition(None, &placeholder, Actor::Device).is_err());
        assert!(check_transition(None, &registered, Actor::Device).is_ok());
        assert!(check_transition(None, &registered, Actor::Admin).is_err());
        assert!(check_transition(Some(&placeholder), &registered, Actor::Device).is_ok());
        assert!(check_transition(Some(&placeholder), &registered, Actor::Admin).is_err());
        assert!(check_transition(Some(&registered), &revoked, Actor::Admin).is_ok());
        assert!(check_transition(Some(&placeholder), &revoked, Actor::Admin).is_ok());
        assert!(check_transition(Some(&registered), &revoked, Actor::Device).is_err());
        for target in [&placeholder, &registered] {
            for actor in [Actor::Device, Actor::Admin] {
                assert!(check_transition(Some(&revoked), target, actor).is_err());
            }
        }
        let rekeyed = DeviceRegistration {
            created_at_unix: 99,
            ..registered.clone()
        };
        assert!(check_transition(Some(&registered), &rekeyed, Actor::Device).is_err());
    }

    #[test]
    fn placeholder_is_claimed_by_the_device_on_its_first_sync() {
        let fx = fixture();
        let device_key = format!("{} laptop@home\n", fx.device_key());

        let preview =
            register_placeholder(&fx.paths, &fx.remote, &device_key, Some("Laptop"), true).unwrap();
        assert_eq!(preview.action, RegistrationAction::Created);
        assert!(preview.dry_run && preview.revision.is_none());
        assert_eq!(fx.list().count, 0, "dry run writes nothing");

        let created =
            register_placeholder(&fx.paths, &fx.remote, &device_key, Some("Laptop"), false)
                .unwrap();
        assert_eq!(created.action, RegistrationAction::Created);
        assert_eq!(created.device_id, fx.device_id);
        let listed = fx.list();
        assert_eq!(listed.count, 1);
        let entry = &listed.registrations[0];
        assert_eq!(entry.status, RegistrationStatus::Placeholder);
        assert_eq!(entry.label.as_deref(), Some("Laptop"));
        assert!(entry.current_device);
        assert_eq!(entry.public_key, fx.device_key(), "comment is dropped");

        let same = register_placeholder(&fx.paths, &fx.remote, &device_key, Some("Laptop"), false)
            .unwrap();
        assert_eq!(same.action, RegistrationAction::Unchanged);
        let relabeled =
            register_placeholder(&fx.paths, &fx.remote, &device_key, Some("Desk"), false).unwrap();
        assert_eq!(relabeled.action, RegistrationAction::Updated);

        assert_eq!(
            fx.self_register().unwrap(),
            Some(SelfRegistrationOutcome::Claimed)
        );
        let claimed = &fx.list().registrations[0];
        assert_eq!(claimed.status, RegistrationStatus::Registered);
        assert_eq!(
            claimed.label.as_deref(),
            Some("Desk"),
            "label survives the claim"
        );
        assert!(claimed.claimed_at_unix.is_some());

        let after = register_placeholder(&fx.paths, &fx.remote, &device_key, None, false).unwrap();
        assert_eq!(after.action, RegistrationAction::Unchanged);
        assert_eq!(after.status, Some(RegistrationStatus::Registered));
    }

    #[test]
    fn first_sync_registers_once_and_later_syncs_stay_quiet() {
        let fx = fixture();
        assert_eq!(
            fx.self_register().unwrap(),
            Some(SelfRegistrationOutcome::Created)
        );
        assert_eq!(
            fx.list().registrations[0].status,
            RegistrationStatus::Registered
        );
        assert_eq!(
            fx.self_register().unwrap(),
            Some(SelfRegistrationOutcome::AlreadyRegistered)
        );
        assert_eq!(fx.list().count, 1, "no duplicate record");
    }

    #[test]
    fn steady_state_decides_from_observed_tips_without_the_remote() {
        let fx = fixture();
        fx.self_register().unwrap();
        let (engine, repository) = fx.engine_and_repo();
        let observed = fx.observed();
        // The remote becomes unreachable; the already-mirrored record and the
        // tips from the sync's own trip are enough.
        fs::remove_dir_all(&fx.remote_path).unwrap();
        assert_eq!(
            self_register_with_store(
                &engine,
                &repository,
                &fx.remote,
                &fx.device,
                Some(&observed),
                10
            )
            .unwrap(),
            Some(SelfRegistrationOutcome::AlreadyRegistered)
        );
    }

    #[test]
    fn a_changed_own_record_is_fetched_only_when_its_tip_moved() {
        let fx = fixture();
        fx.self_register().unwrap();
        revoke_registration(&fx.paths, &fx.remote, &fx.device_id, false).unwrap();
        // A device that missed the revocation has a stale (here: absent)
        // mirror, so the moved tip is fetched and the tombstone reported.
        fx.drop_mirror();
        assert_eq!(
            fx.self_register().unwrap(),
            Some(SelfRegistrationOutcome::Revoked)
        );
    }

    #[test]
    fn a_deleted_record_is_recreated_from_the_observation() {
        let fx = fixture();
        fx.self_register().unwrap();
        unregister_registration(&fx.paths, &fx.remote, &fx.device_id, false).unwrap();
        assert_eq!(fx.list().count, 0);
        assert_eq!(
            fx.self_register().unwrap(),
            Some(SelfRegistrationOutcome::Created),
            "the observation shows no record, so the device registers again"
        );
    }

    #[test]
    fn an_unobserved_sync_prefers_the_mirror_then_falls_back_to_one_lookup() {
        // The admin's own publish left a mirror, so no lookup precedes the claim.
        let fx = fixture();
        register_placeholder(&fx.paths, &fx.remote, &fx.device_key(), None, false).unwrap();
        assert_eq!(
            fx.self_register_unobserved().unwrap(),
            Some(SelfRegistrationOutcome::Claimed)
        );
        assert_eq!(
            fx.list().registrations[0].status,
            RegistrationStatus::Registered
        );

        // With no mirror at all, one explicit lookup finds the record.
        let other = fixture();
        register_placeholder(
            &other.paths,
            &other.remote,
            &other.device_key(),
            None,
            false,
        )
        .unwrap();
        other.drop_mirror();
        assert_eq!(
            other.self_register_unobserved().unwrap(),
            Some(SelfRegistrationOutcome::Claimed)
        );
    }

    #[test]
    fn revoked_tombstone_is_never_overwritten_by_the_device() {
        let fx = fixture();
        fx.self_register().unwrap();
        let revoked = revoke_registration(&fx.paths, &fx.remote, &fx.device_id, false).unwrap();
        assert_eq!(revoked.status, Some(RegistrationStatus::Revoked));
        assert_eq!(
            revoke_registration(&fx.paths, &fx.remote, &fx.device_id, false)
                .unwrap()
                .action,
            RegistrationAction::Unchanged
        );

        let before = fx.list().registrations[0].revision.clone();
        assert_eq!(
            fx.self_register().unwrap(),
            Some(SelfRegistrationOutcome::Revoked)
        );
        assert_eq!(
            fx.list().registrations[0].revision,
            before,
            "no write happened"
        );
        assert_eq!(
            fx.list().registrations[0].status,
            RegistrationStatus::Revoked
        );

        let error =
            register_placeholder(&fx.paths, &fx.remote, &fx.device_key(), None, false).unwrap_err();
        assert!(error.to_string().contains("unregister"));

        let removed = unregister_registration(&fx.paths, &fx.remote, &fx.device_id, false).unwrap();
        assert_eq!(removed.action, RegistrationAction::Removed);
        assert_eq!(fx.list().count, 0);
        assert_eq!(
            unregister_registration(&fx.paths, &fx.remote, &fx.device_id, false)
                .unwrap()
                .action,
            RegistrationAction::Unchanged
        );
        // Starting over is allowed once the tombstone is deliberately removed.
        assert!(register_placeholder(&fx.paths, &fx.remote, &fx.device_key(), None, false).is_ok());
    }

    #[test]
    fn revoking_an_unknown_device_explains_the_alternative() {
        let fx = fixture();
        let error = revoke_registration(&fx.paths, &fx.remote, &fx.device_id, false).unwrap_err();
        assert!(error.to_string().contains("no registration"));
        assert!(
            revoke_registration(&fx.paths, &fx.remote, "01arz3ndektsv4rrffq69g5fav", false)
                .is_err()
        );
        assert!(revoke_registration(&fx.paths, &fx.remote, "vdev1_short", false).is_err());
    }

    #[test]
    fn hostile_records_are_reported_and_never_fatal() {
        let fx = fixture();
        fx.self_register().unwrap();
        let (engine, repository) = fx.engine_and_repo();
        let push = |name: &str, file: &str, payload: &[u8]| {
            let blob = engine.write_blob(&repository, payload).unwrap();
            let tree = engine
                .create_single_file_tree(&repository, file, &blob)
                .unwrap();
            let commit = engine
                .create_commit(&repository, &tree, &[], "x\n")
                .unwrap();
            let reference =
                GitRefName::parse(format!("{REMOTE_REGISTRATION_BRANCH_ROOT}/{name}")).unwrap();
            engine
                .push_ref(&repository, &fx.remote, &commit, &reference, None)
                .unwrap();
        };
        // A record naming a different device than its ref.
        let spoofed_id = format!("vdev1_{}", "a".repeat(52));
        let record = serde_json::to_vec(&sample_record()).unwrap();
        push(&spoofed_id, REGISTRATION_FILE, &record);
        // A wrong file name, a garbage payload, and a non-key ref name.
        push(&format!("vdev1_{}a", "b".repeat(51)), "other.json", &record);
        push(
            &format!("vdev1_{}a", "c".repeat(51)),
            REGISTRATION_FILE,
            b"not json",
        );
        push("01arz3ndektsv4rrffq69g5fav", REGISTRATION_FILE, &record);

        let listed = fx.list();
        assert_eq!(listed.count, 1, "the honest record still lists");
        assert_eq!(listed.registrations[0].device_id, fx.device_id);
        assert_eq!(listed.rejected.len(), 4);
        let reasons = listed
            .rejected
            .iter()
            .map(|entry| entry.reason.as_str())
            .collect::<Vec<_>>()
            .join("|");
        assert!(reasons.contains("different device"), "{reasons}");
        assert!(reasons.contains("registration.json"), "{reasons}");
        assert!(reasons.contains("invalid registration"), "{reasons}");
        assert!(reasons.contains("key-derived"), "{reasons}");
    }

    #[test]
    fn offline_listing_reads_the_last_fetched_copies_without_the_remote() {
        let fx = fixture();
        fx.self_register().unwrap();
        assert_eq!(
            fx.list().registrations[0].source,
            RegistrationSource::Remote
        );

        let offline = list_registrations_with_current(&fx.paths, &fx.remote, false, None).unwrap();
        assert_eq!(offline.observation, RegistrationObservation::NotRequested);
        assert_eq!(
            offline.registrations[0].source,
            RegistrationSource::LocalMirror
        );

        fs::remove_dir_all(&fx.remote_path).unwrap();
        let unavailable =
            list_registrations_with_current(&fx.paths, &fx.remote, true, None).unwrap();
        assert_eq!(
            unavailable.observation,
            RegistrationObservation::Unavailable
        );
        assert_eq!(unavailable.count, 1);
    }

    #[test]
    fn missing_identity_registers_nothing() {
        let fx = fixture();
        let (engine, repository) = fx.engine_and_repo();
        let empty = DeviceIdentityStore::at(fx.paths.vault_root().join("no-identity"));
        let outcome =
            self_register_with_store(&engine, &repository, &fx.remote, &empty, Some(&[]), 1);
        assert_eq!(outcome.unwrap(), None);
        assert_eq!(fx.list().count, 0);
    }

    #[test]
    fn registration_never_touches_the_work_tree() {
        let fx = fixture();
        fx.self_register().unwrap();
        // Plain Git vaults replicate the whole work tree, so registering
        // writes only refs: no files, and no `.vulcan/` directory.
        let entries = fs::read_dir(fx.paths.vault_root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [std::ffi::OsString::from(".git")]);
    }
}
