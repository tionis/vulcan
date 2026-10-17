//! Trusted-commit execution authorization (Roadmap 10.8,
//! `docs/specs/scheduled-execution.md`, "Trusted-commit v1").
//!
//! Under `trusted-commit` a job runs only when its execution definition is
//! the one an allowed signer signed. Definitions are read from Git objects,
//! never the working tree; signatures are SSH commit signatures verified in
//! process against a dedicated, protected allowed-signers file; and a
//! per-device checkpoint, kept in the schedule ledger, rejects rollbacks,
//! forks, and old definitions copied back in. The resolver is synchronous and
//! never contacts a remote.

use crate::schedules::ScheduleLedger;
use crate::AppError;
use serde::{Deserialize, Serialize};
use ssh_key::{PublicKey, SshSig};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use vulcan_core::schedules::{parse_schedules, ScheduleSet};
use vulcan_core::VaultPaths;

/// The name a trusted-commit decision reports.
pub const TRUSTED_COMMIT_POLICY: &str = "trusted-commit";
/// The SSHSIG namespace Git uses for commit signatures.
const GIT_SIGNATURE_NAMESPACE: &str = "git";
const MAX_SIGNERS_BYTES: u64 = 64 * 1024;
/// Commits walked between the checkpoint and the tip before giving up.
const MAX_WALK_COMMITS: usize = 100_000;
const DEFAULT_SOURCE: &str = "HEAD";

/// The device-local trust configuration of one registered vault
/// (`[vault.execution_trust]` in the daemon configuration). Its presence
/// selects trusted-commit; without it the vault uses repository trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTrustConfig {
    pub policy: ExecutionTrustPolicy,
    /// A protected OpenSSH allowed-signers file outside the vault, listing
    /// execution signers only.
    pub allowed_signers: PathBuf,
    /// Which principals of that file may authorize.
    pub principals: Vec<String>,
    /// The ref whose tip is resolved.
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    DEFAULT_SOURCE.to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionTrustPolicy {
    TrustedCommit,
}

impl ExecutionTrustConfig {
    pub fn new(
        allowed_signers: PathBuf,
        principals: Vec<String>,
        source: Option<String>,
    ) -> Result<Self, AppError> {
        let config = Self {
            policy: ExecutionTrustPolicy::TrustedCommit,
            allowed_signers,
            principals,
            source: source.unwrap_or_else(default_source),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if !self.allowed_signers.is_absolute() {
            return Err(AppError::operation(
                "the allowed-signers path must be absolute",
            ));
        }
        if self.principals.is_empty() {
            return Err(AppError::operation(
                "trusted-commit needs at least one principal",
            ));
        }
        for principal in &self.principals {
            if principal.is_empty()
                || principal
                    .chars()
                    .any(|character| character.is_whitespace() || ",*?!\"".contains(character))
            {
                return Err(AppError::operation(format!(
                    "`{principal}` is not a principal: no spaces, commas, quotes, or patterns"
                )));
            }
        }
        if self.source.is_empty() || self.source.starts_with('-') {
            return Err(AppError::operation(format!(
                "`{}` is not a ref",
                self.source
            )));
        }
        Ok(())
    }
}

/// Where the vault's repository is, as the registration says.
#[derive(Debug, Clone)]
pub struct TrustRepository {
    pub vault: PathBuf,
    pub git_dir: Option<PathBuf>,
    pub work_tree: Option<PathBuf>,
}

/// Why a job is or is not authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustState {
    Authorized,
    /// An unsigned commit changed the job after the last signature.
    UnsignedChange,
    /// Signed commits on merged branches disagree about the job.
    Conflicted,
    /// History no longer contains the accepted checkpoint.
    RolledBack,
    /// No checkpoint is recorded on this device.
    NoCheckpoint,
    /// No allowed signer signed anything that authorizes the job.
    Unsigned,
    /// The vault is not a Git repository, or its source ref is missing.
    Unsupported,
    /// The trust configuration or allowed-signers file is unusable.
    Misconfigured,
}

impl TrustState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authorized => "authorized",
            Self::UnsignedChange => "unsigned-change",
            Self::Conflicted => "conflicted",
            Self::RolledBack => "rolled-back",
            Self::NoCheckpoint => "no-checkpoint",
            Self::Unsigned => "unsigned",
            Self::Unsupported => "unsupported",
            Self::Misconfigured => "misconfigured",
        }
    }
}

/// A commit an allowed signer signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignedCommit {
    pub commit: String,
    pub signer: String,
}

/// The accepted checkpoint of one vault on this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustCheckpoint {
    pub commit: String,
    pub signer: String,
    pub policy_generation: String,
    pub accepted_ms: i64,
    /// `enabled`, `advanced`, or `accepted`.
    pub how: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobTrust {
    pub job_id: String,
    pub state: TrustState,
    pub detail: String,
    /// The working tree's definition differs from the tip's: an uncommitted
    /// edit that has no effect until committed and signed.
    pub pending: bool,
}

/// One resolution of a vault's trust state.
#[derive(Debug, Clone, Serialize)]
pub struct TrustResolution {
    pub policy: &'static str,
    pub source: String,
    pub principals: Vec<String>,
    pub allowed_signers: String,
    pub policy_generation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<TrustCheckpoint>,
    pub frontier: Vec<SignedCommit>,
    /// Set when no job of the vault may run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<VaultBlock>,
    pub jobs: Vec<JobTrust>,
    /// Allowed-signers lines that were ignored, and why.
    pub signer_diagnostics: Vec<String>,
    /// The schedules at the tip, which authorized jobs run from.
    #[serde(skip)]
    pub schedules: ScheduleSet,
    /// The shared configuration at the tip, for the run's profile and target
    /// configuration.
    #[serde(skip)]
    pub signed_config: Option<toml::Value>,
    /// A frontier commit that may become the checkpoint.
    #[serde(skip)]
    pub advance: Option<SignedCommit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultBlock {
    pub state: TrustState,
    pub detail: String,
}

impl TrustResolution {
    #[must_use]
    pub fn job(&self, id: &str) -> Option<&JobTrust> {
        self.jobs.iter().find(|job| job.job_id == id)
    }

    /// Whether `job_id` may run, and if not, why.
    pub fn authorize(&self, job_id: &str) -> Result<(), String> {
        if let Some(block) = &self.blocked {
            return Err(format!(
                "trusted-commit: {} ({})",
                block.detail,
                block.state.as_str()
            ));
        }
        match self.job(job_id) {
            Some(job) if job.state == TrustState::Authorized => Ok(()),
            Some(job) => Err(format!(
                "trusted-commit: {} ({})",
                job.detail,
                job.state.as_str()
            )),
            None => Err(format!(
                "trusted-commit: job `{job_id}` is not defined at the resolved commit"
            )),
        }
    }

    fn blocked(mut self, state: TrustState, detail: impl Into<String>) -> Self {
        self.blocked = Some(VaultBlock {
            state,
            detail: detail.into(),
        });
        self
    }
}

/// An allowed-signers entry that may authorize.
#[derive(Debug, Clone)]
struct AllowedSigner {
    /// The configured principal this entry carries.
    principal: String,
    key: PublicKey,
}

/// Resolves `repository` under `config`. Reads the checkpoint from `ledger`
/// but never writes it; [`record_advance`] does that.
pub fn resolve_trust(
    config: &ExecutionTrustConfig,
    repository: &TrustRepository,
    ledger: &ScheduleLedger,
    vault_key: &str,
) -> TrustResolution {
    resolve_trust_refusing(
        config,
        repository,
        ledger,
        vault_key,
        &device_keys(&repository.vault),
    )
}

/// [`resolve_trust`] refusing `refused` keys as signers.
fn resolve_trust_refusing(
    config: &ExecutionTrustConfig,
    repository: &TrustRepository,
    ledger: &ScheduleLedger,
    vault_key: &str,
    refused: &[PublicKey],
) -> TrustResolution {
    let mut resolution = TrustResolution {
        policy: TRUSTED_COMMIT_POLICY,
        source: config.source.clone(),
        principals: config.principals.clone(),
        allowed_signers: config.allowed_signers.display().to_string(),
        policy_generation: String::new(),
        tip: None,
        checkpoint: None,
        frontier: Vec::new(),
        blocked: None,
        jobs: Vec::new(),
        signer_diagnostics: Vec::new(),
        schedules: ScheduleSet::default(),
        signed_config: None,
        advance: None,
    };
    if let Err(error) = config.validate() {
        return resolution.blocked(TrustState::Misconfigured, error.to_string());
    }
    let git = match Git::open(repository) {
        Ok(git) => git,
        Err(detail) => return resolution.blocked(TrustState::Unsupported, detail),
    };
    let (signers, generation) = match load_allowed_signers(config, repository, &git, refused) {
        Ok(loaded) => loaded,
        Err(detail) => return resolution.blocked(TrustState::Misconfigured, detail),
    };
    resolution.policy_generation = generation;
    resolution.signer_diagnostics = signers.diagnostics;
    let signers = signers.signers;
    let tip = match git.resolve_commit(&config.source) {
        Ok(tip) => tip,
        Err(detail) => return resolution.blocked(TrustState::Unsupported, detail),
    };
    resolution.tip = Some(tip.clone());
    let tip_config = match git.config_at(&tip) {
        Ok(config) => config,
        Err(detail) => return resolution.blocked(TrustState::Unsupported, detail),
    };
    resolution.schedules = parse_schedules(tip_config.source.as_deref().unwrap_or(""));
    resolution.signed_config.clone_from(&tip_config.table);
    let checkpoint = match ledger.trust_checkpoint(vault_key) {
        Ok(checkpoint) => checkpoint,
        Err(error) => return resolution.blocked(TrustState::Misconfigured, error.to_string()),
    };
    resolution.checkpoint.clone_from(&checkpoint);

    // Signed commits since the checkpoint (or in all history without one).
    let base = checkpoint
        .as_ref()
        .map(|checkpoint| checkpoint.commit.clone());
    if let Some(base) = &base {
        match git.is_ancestor(base, &tip) {
            Ok(true) => {}
            Ok(false) => {
                resolution = resolution.blocked(
                    TrustState::RolledBack,
                    format!(
                        "history at `{}` no longer contains the accepted checkpoint {}; run `vulcan schedule trust accept` to move past it",
                        config.source,
                        short(base)
                    ),
                );
                resolution.jobs = job_states(&resolution, &git, &tip_config, &[], repository);
                return resolution;
            }
            Err(detail) => return resolution.blocked(TrustState::Unsupported, detail),
        }
    }
    let frontier = match authorizing_frontier(&git, &tip, base.as_deref(), &signers) {
        Ok(Some(frontier)) => frontier,
        Ok(None) => {
            resolution = resolution.blocked(
                TrustState::Unsigned,
                format!(
                    "the checkpoint {}'s signer is no longer trusted; an allowed signer must sign a newer commit",
                    short(base.as_deref().unwrap_or_default())
                ),
            );
            resolution.jobs = job_states(&resolution, &git, &tip_config, &[], repository);
            return resolution;
        }
        Err(detail) => return resolution.blocked(TrustState::Unsupported, detail),
    };
    resolution.frontier.clone_from(&frontier);
    if frontier.len() == 1 && base.as_deref() != Some(frontier[0].commit.as_str()) {
        resolution.advance = Some(frontier[0].clone());
    }
    if checkpoint.is_none() {
        resolution.jobs = job_states(&resolution, &git, &tip_config, &frontier, repository);
        return resolution.blocked(
            TrustState::NoCheckpoint,
            "no trust checkpoint is recorded on this device; run `vulcan schedule trust enable` or `vulcan schedule trust accept`",
        );
    }
    resolution.jobs = job_states(&resolution, &git, &tip_config, &frontier, repository);
    resolution
}

/// The signed commits since `base` that no other one descends from, or
/// `base` itself when none was signed since. `None` when `base`'s own signer
/// is no longer trusted: falling back to an older signature would be a
/// rollback.
fn authorizing_frontier(
    git: &Git,
    tip: &str,
    base: Option<&str>,
    signers: &[AllowedSigner],
) -> Result<Option<Vec<SignedCommit>>, String> {
    let signed = git.signed_since(tip, base, signers)?;
    if !signed.is_empty() {
        return git.independent(&signed).map(Some);
    }
    let Some(base) = base else {
        return Ok(Some(Vec::new()));
    };
    Ok(git.signer_of(base, signers)?.map(|signer| {
        vec![SignedCommit {
            commit: base.to_string(),
            signer,
        }]
    }))
}

/// Records `resolution`'s frontier as the new checkpoint when it is a single
/// signed commit past the old one. Returns whether it advanced.
pub fn record_advance(
    resolution: &TrustResolution,
    ledger: &ScheduleLedger,
    vault_key: &str,
    now_ms: i64,
) -> Result<bool, AppError> {
    if resolution.checkpoint.is_none() || resolution.blocked.is_some() {
        return Ok(false);
    }
    let Some(advance) = &resolution.advance else {
        return Ok(false);
    };
    ledger.set_trust_checkpoint(
        vault_key,
        &TrustCheckpoint {
            commit: advance.commit.clone(),
            signer: advance.signer.clone(),
            policy_generation: resolution.policy_generation.clone(),
            accepted_ms: now_ms,
            how: "advanced".to_string(),
        },
    )?;
    Ok(true)
}

/// Pins `commit` (or the resolved frontier when `None`) as the checkpoint, as
/// `trust enable` and `trust accept` do. The commit must be signed by an
/// allowed signer and reachable from the source tip.
pub fn pin_checkpoint(
    config: &ExecutionTrustConfig,
    repository: &TrustRepository,
    ledger: &ScheduleLedger,
    vault_key: &str,
    pin: &CheckpointPin<'_>,
) -> Result<TrustCheckpoint, AppError> {
    pin_checkpoint_refusing(
        config,
        repository,
        ledger,
        vault_key,
        pin,
        &device_keys(&repository.vault),
    )
}

/// What [`pin_checkpoint`] pins, and how.
pub struct CheckpointPin<'a> {
    /// The commit to pin; `None` pins the single signed frontier.
    pub commit: Option<&'a str>,
    /// `enabled` or `accepted`.
    pub how: &'a str,
    pub now_ms: i64,
    pub dry_run: bool,
}

fn pin_checkpoint_refusing(
    config: &ExecutionTrustConfig,
    repository: &TrustRepository,
    ledger: &ScheduleLedger,
    vault_key: &str,
    pin: &CheckpointPin<'_>,
    refused: &[PublicKey],
) -> Result<TrustCheckpoint, AppError> {
    config.validate()?;
    let git = Git::open(repository).map_err(AppError::operation)?;
    let (signers, generation) =
        load_allowed_signers(config, repository, &git, refused).map_err(AppError::operation)?;
    let tip = git
        .resolve_commit(&config.source)
        .map_err(AppError::operation)?;
    let signed = if let Some(commit) = pin.commit {
        named_checkpoint(&git, config, &tip, commit, &signers.signers)?
    } else {
        newest_checkpoint(&git, config, &tip, &signers.signers)?
    };
    let checkpoint = TrustCheckpoint {
        commit: signed.commit,
        signer: signed.signer,
        policy_generation: generation,
        accepted_ms: pin.now_ms,
        how: pin.how.to_string(),
    };
    if !pin.dry_run {
        ledger.set_trust_checkpoint(vault_key, &checkpoint)?;
    }
    Ok(checkpoint)
}

/// `commit`, when it is in the source's history and an allowed signer signed it.
fn named_checkpoint(
    git: &Git,
    config: &ExecutionTrustConfig,
    tip: &str,
    commit: &str,
    signers: &[AllowedSigner],
) -> Result<SignedCommit, AppError> {
    let commit = git.resolve_commit(commit).map_err(AppError::operation)?;
    if !git.is_ancestor(&commit, tip).map_err(AppError::operation)? {
        return Err(AppError::operation(format!(
            "{} is not in the history of `{}`",
            short(&commit),
            config.source
        )));
    }
    let signer = git
        .signer_of(&commit, signers)
        .map_err(AppError::operation)?
        .ok_or_else(|| {
            AppError::operation(format!(
                "{} is not signed by an allowed signer",
                short(&commit)
            ))
        })?;
    Ok(SignedCommit { commit, signer })
}

/// The single newest signed commit in the source's history.
fn newest_checkpoint(
    git: &Git,
    config: &ExecutionTrustConfig,
    tip: &str,
    signers: &[AllowedSigner],
) -> Result<SignedCommit, AppError> {
    let signed = git
        .signed_since(tip, None, signers)
        .map_err(AppError::operation)?;
    let frontier = git.independent(&signed).map_err(AppError::operation)?;
    match frontier.as_slice() {
        [] => Err(AppError::operation(format!(
            "no commit in the history of `{}` is signed by an allowed signer",
            config.source
        ))),
        [only] => Ok(only.clone()),
        _ => Err(AppError::operation(format!(
            "signed commits on merged branches disagree ({}); name the commit to accept",
            frontier
                .iter()
                .map(|signed| short(&signed.commit))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Each job's state at the tip against the frontier.
fn job_states(
    resolution: &TrustResolution,
    git: &Git,
    tip_config: &CommitConfig,
    frontier: &[SignedCommit],
    repository: &TrustRepository,
) -> Vec<JobTrust> {
    let tip_digests = definition_digests(tip_config);
    let frontier_digests = frontier
        .iter()
        .map(|signed| {
            git.config_at(&signed.commit)
                .map(|config| definition_digests(&config))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let working = std::fs::read_to_string(VaultPaths::new(&repository.vault).config_file())
        .ok()
        .map(|source| definition_digests(&CommitConfig::parse(Some(source))))
        .unwrap_or_default();
    let mut jobs = Vec::new();
    for (id, digest) in &tip_digests {
        let pending = working.get(id) != Some(digest);
        let (state, detail) = if let Some(block) = &resolution.blocked {
            (block.state, block.detail.clone())
        } else if frontier.is_empty() {
            (
                TrustState::Unsigned,
                "no allowed signer has signed a commit".to_string(),
            )
        } else {
            let signed = frontier_digests
                .iter()
                .map(|digests| digests.get(id).cloned().unwrap_or_default())
                .collect::<BTreeSet<_>>();
            if signed.len() > 1 {
                (
                    TrustState::Conflicted,
                    format!("signed commits disagree about job `{id}`"),
                )
            } else if signed.iter().next() == Some(digest) {
                (
                    TrustState::Authorized,
                    format!(
                        "signed by {} in {}",
                        frontier
                            .iter()
                            .map(|signed| signed.signer.as_str())
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect::<Vec<_>>()
                            .join(", "),
                        frontier
                            .iter()
                            .map(|signed| short(&signed.commit))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            } else {
                (
                    TrustState::UnsignedChange,
                    format!(
                        "job `{id}` changed after the last signed commit; it needs a new signature"
                    ),
                )
            }
        };
        jobs.push(JobTrust {
            job_id: id.clone(),
            state,
            detail,
            pending,
        });
    }
    jobs
}

/// The shared configuration at one commit.
#[derive(Debug, Clone, Default)]
struct CommitConfig {
    source: Option<String>,
    table: Option<toml::Value>,
    /// The file exists but does not parse.
    unparseable: bool,
}

impl CommitConfig {
    fn parse(source: Option<String>) -> Self {
        let (table, unparseable) = match source.as_deref() {
            None => (None, false),
            Some(text) => match toml::from_str::<toml::Table>(text) {
                Ok(table) => (Some(toml::Value::Table(table)), false),
                Err(_) => (None, true),
            },
        };
        Self {
            source,
            table,
            unparseable,
        }
    }
}

/// Each job's execution-definition digest: its `[[schedules.jobs]]` entry,
/// the shared definition of the profile its `authority` names, and the
/// sections its target declares as execution configuration.
fn definition_digests(config: &CommitConfig) -> BTreeMap<String, String> {
    let mut digests = BTreeMap::new();
    if config.unparseable {
        // Every job of an unparseable file is unauthorized; the loader blocks
        // the section anyway.
        return digests;
    }
    let Some(table) = &config.table else {
        return digests;
    };
    let schedules = table.get("schedules");
    let version = schedules
        .and_then(|schedules| schedules.get("version"))
        .cloned();
    let Some(jobs) = schedules
        .and_then(|schedules| schedules.get("jobs"))
        .and_then(toml::Value::as_array)
    else {
        return digests;
    };
    let mut by_id = BTreeMap::<String, Vec<&toml::Value>>::new();
    for job in jobs {
        if let Some(id) = job.get("id").and_then(toml::Value::as_str) {
            by_id.entry(id.to_string()).or_default().push(job);
        }
    }
    for (id, entries) in by_id {
        let authority = entries[0]
            .get("authority")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        let profile = table
            .get("permissions")
            .and_then(|permissions| permissions.get("profiles"))
            .and_then(|profiles| profiles.get(authority))
            .cloned();
        let target = entries[0]
            .get("target")
            .and_then(|target| target.get("builtin"))
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        let target_config = match target {
            "batch-commit" => table.get("git").cloned(),
            _ => None,
        };
        let definition = serde_json::json!({
            "schedules_version": version,
            "jobs": entries,
            "authority_profile": profile,
            "target_config": target_config,
        });
        let canonical = serde_json_canonicalizer::to_vec(&definition).unwrap_or_default();
        digests.insert(id, blake3::hash(&canonical).to_hex().to_string());
    }
    digests
}

struct LoadedSigners {
    signers: Vec<AllowedSigner>,
    diagnostics: Vec<String>,
}

/// Reads and checks the allowed-signers file. Returns the usable entries and
/// the policy generation: a digest of the configuration and the file.
fn load_allowed_signers(
    config: &ExecutionTrustConfig,
    repository: &TrustRepository,
    git: &Git,
    refused: &[PublicKey],
) -> Result<(LoadedSigners, String), String> {
    let path = &config.allowed_signers;
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| format!("cannot open allowed signers {}: {error}", path.display()))?;
    for root in [Some(&repository.vault), repository.work_tree.as_ref()]
        .into_iter()
        .flatten()
        .chain(git.work_tree.as_ref())
    {
        if let Ok(root) = std::fs::canonicalize(root) {
            if canonical.starts_with(&root) {
                return Err(format!(
                    "allowed signers {} must be outside the vault and its Git work tree",
                    path.display()
                ));
            }
        }
    }
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("cannot open allowed signers {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_SIGNERS_BYTES {
        return Err(format!(
            "allowed signers {} must be a regular file of at most {MAX_SIGNERS_BYTES} bytes",
            path.display()
        ));
    }
    check_protected(&canonical, &metadata)?;
    let text = std::fs::read_to_string(&canonical)
        .map_err(|error| format!("cannot read allowed signers {}: {error}", path.display()))?;
    let loaded = parse_allowed_signers(&text, &config.principals);
    for signer in &loaded.signers {
        if refused
            .iter()
            .any(|key| key.key_data() == signer.key.key_data())
        {
            return Err(format!(
                "allowed signers {} lists a device key; automatic sync commits must never authorize jobs, so use a separate execution signer key",
                path.display()
            ));
        }
    }
    let generation = blake3::hash(
        &serde_json_canonicalizer::to_vec(&serde_json::json!({
            "config": config,
            "signers": text,
        }))
        .unwrap_or_default(),
    )
    .to_hex()
    .to_string();
    Ok((loaded, generation))
}

#[cfg(unix)]
fn check_protected(path: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!(
            "allowed signers {} must be owned by the current user",
            path.display()
        ));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "allowed signers {} must not be writable by group or others",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_protected(_path: &Path, _metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

/// Keys that must never authorize: this device's identity key and every
/// device key in the vault's mirrored registrations. Best effort: a key that
/// cannot be read is simply not compared.
fn device_keys(vault: &Path) -> Vec<PublicKey> {
    let mut keys = Vec::new();
    if let Ok(store) = crate::device_identity::DeviceIdentityStore::user_default() {
        if let Ok(key) = store.public_key() {
            keys.extend(PublicKey::from_openssh(&key).ok());
        }
    }
    if let Ok(remote) = vulcan_sync::GitRemote::parse("origin") {
        if let Ok(report) =
            crate::sync_registration::list_registrations(&VaultPaths::new(vault), &remote, false)
        {
            for registration in report.registrations {
                keys.extend(PublicKey::from_openssh(&registration.public_key).ok());
            }
        }
    }
    keys
}

/// Parses OpenSSH allowed-signers lines, keeping the entries one of
/// `principals` may use. Unsupported lines are reported and ignored, which
/// can only reduce what is trusted.
fn parse_allowed_signers(text: &str, principals: &[String]) -> LoadedSigners {
    let mut signers = Vec::new();
    let mut diagnostics = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let number = index + 1;
        match parse_signer_line(line, principals) {
            Ok(Some(signer)) => signers.push(signer),
            Ok(None) => {}
            Err(reason) => diagnostics.push(format!("line {number}: {reason}")),
        }
    }
    LoadedSigners {
        signers,
        diagnostics,
    }
}

fn parse_signer_line(line: &str, principals: &[String]) -> Result<Option<AllowedSigner>, String> {
    let tokens = tokenize(line);
    let mut tokens = tokens.iter().map(String::as_str);
    let entry_principals = tokens.next().ok_or("empty line")?;
    let mut next = tokens.next().ok_or("no key")?;
    let mut namespaces = None;
    if next.parse::<ssh_key::Algorithm>().is_err() {
        for option in split_options(next) {
            let (name, value) = option
                .split_once('=')
                .map_or((option.as_str(), None), |(name, value)| {
                    (name, Some(value.trim_matches('"')))
                });
            match name.to_ascii_lowercase().as_str() {
                "namespaces" => {
                    namespaces = Some(
                        value
                            .unwrap_or_default()
                            .split(',')
                            .map(str::to_string)
                            .collect::<Vec<_>>(),
                    );
                }
                "cert-authority" | "valid-after" | "valid-before" => {
                    return Err(format!("`{name}` is not supported in v1; line ignored"));
                }
                other => return Err(format!("unknown option `{other}`; line ignored")),
            }
        }
        next = tokens.next().ok_or("no key")?;
    }
    let algorithm = next;
    let data = tokens.next().ok_or("no key data")?;
    let key = PublicKey::from_openssh(&format!("{algorithm} {data}"))
        .map_err(|error| format!("invalid key: {error}"))?;
    if !matches!(key.algorithm(), ssh_key::Algorithm::Ed25519) {
        return Err(format!(
            "{} keys are not supported in v1; line ignored",
            key.algorithm()
        ));
    }
    if namespaces.as_ref().is_some_and(|namespaces| {
        !namespaces
            .iter()
            .any(|name| name == GIT_SIGNATURE_NAMESPACE)
    }) {
        return Ok(None);
    }
    let mut principal = None;
    for candidate in entry_principals.split(',') {
        if candidate.contains(['*', '?', '!']) {
            return Err("principal patterns are not supported; line ignored".to_string());
        }
        if principal.is_none() && principals.iter().any(|wanted| wanted == candidate) {
            principal = Some(candidate.to_string());
        }
    }
    Ok(principal.map(|principal| AllowedSigner { principal, key }))
}

/// Splits on whitespace outside double quotes.
fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in line.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            character if character.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Splits an options field on commas outside double quotes.
fn split_options(field: &str) -> Vec<String> {
    let mut options = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in field.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            ',' if !quoted => options.push(std::mem::take(&mut current)),
            character => current.push(character),
        }
    }
    options.push(current);
    options
}

/// The signer of a raw commit object, if an allowed signer signed it.
fn verify_commit(object: &[u8], signers: &[AllowedSigner]) -> Option<String> {
    let (payload, signature) = split_signature(object)?;
    let signature = SshSig::from_pem(signature).ok()?;
    if signature.namespace() != GIT_SIGNATURE_NAMESPACE {
        return None;
    }
    signers
        .iter()
        .find(|signer| {
            signer.key.key_data() == signature.public_key()
                && signer
                    .key
                    .verify(GIT_SIGNATURE_NAMESPACE, &payload, &signature)
                    .is_ok()
        })
        .map(|signer| signer.principal.clone())
}

/// Splits a commit object into the signed payload (the object without its
/// signature header) and the armored signature, as Git does.
fn split_signature(object: &[u8]) -> Option<(Vec<u8>, String)> {
    let header_end = object
        .windows(2)
        .position(|window| window == b"\n\n")
        .map_or(object.len(), |position| position + 1);
    let (header, body) = object.split_at(header_end);
    let mut payload = Vec::with_capacity(object.len());
    let mut signature: Option<String> = None;
    let mut in_signature = false;
    for line in header.split_inclusive(|byte| *byte == b'\n') {
        if in_signature {
            if let Some(continuation) = line.strip_prefix(b" ") {
                let text = String::from_utf8_lossy(continuation);
                let signature = signature.as_mut().expect("signature started");
                signature.push_str(&text);
                continue;
            }
            in_signature = false;
        }
        let gpgsig = line
            .strip_prefix(b"gpgsig ")
            .or_else(|| line.strip_prefix(b"gpgsig-sha256 "));
        if let Some(first) = gpgsig {
            if signature.is_some() {
                // Two signature headers: refuse rather than guess.
                return None;
            }
            signature = Some(String::from_utf8_lossy(first).into_owned());
            in_signature = true;
            continue;
        }
        payload.extend_from_slice(line);
    }
    payload.extend_from_slice(body);
    signature.map(|signature| (payload, signature))
}

fn short(commit: &str) -> String {
    commit.chars().take(12).collect()
}

/// Read-only Git access to one repository. Never runs hooks, filters, or a
/// configured verifier: only plumbing that reads objects and refs.
struct Git {
    arguments: Vec<String>,
    /// The vault's path inside the repository, with a trailing `/`, or empty.
    prefix: String,
    work_tree: Option<PathBuf>,
}

impl Git {
    fn open(repository: &TrustRepository) -> Result<Self, String> {
        let mut arguments = vec!["-c".to_string(), "core.fsmonitor=false".to_string()];
        if let Some(git_dir) = &repository.git_dir {
            arguments.push("--git-dir".to_string());
            arguments.push(git_dir.display().to_string());
            arguments.push("--work-tree".to_string());
            arguments.push(
                repository
                    .work_tree
                    .as_ref()
                    .unwrap_or(&repository.vault)
                    .display()
                    .to_string(),
            );
        } else {
            arguments.push("-C".to_string());
            arguments.push(repository.vault.display().to_string());
        }
        let mut git = Self {
            arguments,
            prefix: String::new(),
            work_tree: None,
        };
        let top = git.text(&["rev-parse", "--show-toplevel"]).map_err(|_| {
            "trusted-commit needs a Git repository; this vault has none".to_string()
        })?;
        git.work_tree = Some(PathBuf::from(top.trim()));
        let root = std::fs::canonicalize(top.trim()).map_err(|error| error.to_string())?;
        let vault = std::fs::canonicalize(&repository.vault).map_err(|error| error.to_string())?;
        let relative = vault.strip_prefix(&root).map_err(|_| {
            format!(
                "the vault {} is not inside its Git work tree",
                repository.vault.display()
            )
        })?;
        for component in relative.components() {
            git.prefix
                .push_str(&component.as_os_str().to_string_lossy());
            git.prefix.push('/');
        }
        Ok(git)
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .args(&self.arguments)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null());
        command
    }

    fn output(&self, arguments: &[&str]) -> Result<std::process::Output, String> {
        self.command()
            .args(arguments)
            .output()
            .map_err(|error| format!("cannot run git: {error}"))
    }

    fn text(&self, arguments: &[&str]) -> Result<String, String> {
        let output = self.output(arguments)?;
        if !output.status.success() {
            return Err(format!(
                "git {} failed: {}",
                arguments.first().copied().unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        String::from_utf8(output.stdout).map_err(|error| error.to_string())
    }

    fn resolve_commit(&self, name: &str) -> Result<String, String> {
        self.text(&[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("{name}^{{commit}}"),
        ])
        .map(|text| text.trim().to_string())
        .map_err(|_| format!("`{name}` does not name a commit"))
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, String> {
        let output = self.output(&["merge-base", "--is-ancestor", ancestor, descendant])?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(format!(
                "git merge-base failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
        }
    }

    /// The shared configuration file at `commit`.
    fn config_at(&self, commit: &str) -> Result<CommitConfig, String> {
        let spec = format!("{commit}:{}.vulcan/config.toml", self.prefix);
        let output = self.output(&["cat-file", "blob", &spec])?;
        if !output.status.success() {
            // Missing at that commit: no jobs.
            return Ok(CommitConfig::default());
        }
        Ok(CommitConfig::parse(Some(
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )))
    }

    fn signer_of(&self, commit: &str, signers: &[AllowedSigner]) -> Result<Option<String>, String> {
        let output = self.output(&["cat-file", "commit", commit])?;
        if !output.status.success() {
            return Err(format!("cannot read commit {}", short(commit)));
        }
        Ok(verify_commit(&output.stdout, signers))
    }

    /// Commits reachable from `tip` and not from `base` that an allowed
    /// signer signed, read through one `cat-file --batch`.
    fn signed_since(
        &self,
        tip: &str,
        base: Option<&str>,
        signers: &[AllowedSigner],
    ) -> Result<Vec<SignedCommit>, String> {
        let mut arguments = vec!["rev-list", tip];
        let exclude = base.map(|base| format!("^{base}"));
        if let Some(exclude) = &exclude {
            arguments.push(exclude);
        }
        let commits = self
            .text(&arguments)?
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if commits.len() > MAX_WALK_COMMITS {
            return Err(format!(
                "more than {MAX_WALK_COMMITS} commits since the checkpoint; accept a newer one with `vulcan schedule trust accept`"
            ));
        }
        if commits.is_empty() || signers.is_empty() {
            return Ok(Vec::new());
        }
        let mut child = self
            .command()
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot run git: {error}"))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let input = commits.join("\n") + "\n";
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut signed = Vec::new();
        for commit in &commits {
            let mut header = String::new();
            reader
                .read_line(&mut header)
                .map_err(|error| error.to_string())?;
            let size = header
                .split_whitespace()
                .nth(2)
                .and_then(|size| size.parse::<usize>().ok())
                .ok_or_else(|| format!("cannot read commit {}", short(commit)))?;
            let mut object = vec![0; size + 1];
            reader
                .read_exact(&mut object)
                .map_err(|error| error.to_string())?;
            object.pop();
            if let Some(signer) = verify_commit(&object, signers) {
                signed.push(SignedCommit {
                    commit: commit.clone(),
                    signer,
                });
            }
        }
        let _ = writer.join();
        let _ = child.wait();
        Ok(signed)
    }

    /// The signed commits that are not ancestors of another one.
    fn independent(&self, signed: &[SignedCommit]) -> Result<Vec<SignedCommit>, String> {
        if signed.len() <= 1 {
            return Ok(signed.to_vec());
        }
        let mut arguments = vec!["merge-base", "--independent"];
        arguments.extend(signed.iter().map(|signed| signed.commit.as_str()));
        let independent = self
            .text(&arguments)?
            .lines()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        Ok(signed
            .iter()
            .filter(|signed| independent.contains(&signed.commit))
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests;
