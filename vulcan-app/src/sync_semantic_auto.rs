//! Debounced, finite semantic-history automation for daemons and CI schedulers.

use crate::durable_file;
use crate::sync::{GitRefName, GitRemote, SyncCancellationToken};
use crate::sync_semantic::{
    apply_semantic_plan_with_state_store, create_semantic_plan_with_provider_and_state_store,
    create_semantic_plan_with_state_store, publish_semantic_plan_with_state_store,
    SemanticAgentProvider, SemanticApplyReport, SemanticGrouping, SemanticPlanOptions,
    SemanticPlanReport, SemanticPublishReport,
};
use crate::sync_state::{repository_state_key, SyncStateStore};
use crate::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use vulcan_core::VaultPaths;
use vulcan_sync::{GitEngine, GitOid, GitSyncOptions, GitSyncRefs};

pub const SEMANTIC_AUTO_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticAutoOptions {
    pub semantic_ref: GitRefName,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub grouping: SemanticGrouping,
    pub agent: bool,
    pub publish: bool,
    pub quiet_seconds: u64,
    pub maximum_wait_seconds: u64,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticAutoOutcome {
    Deferred,
    UpToDate,
    Preview,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticAutoReport {
    pub version: u32,
    pub dry_run: bool,
    pub outcome: SemanticAutoOutcome,
    pub source_revision: String,
    pub target_revision: String,
    pub stable_for_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_eligible_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<SemanticPlanReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application: Option<SemanticApplyReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<SemanticPublishReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SemanticAutoState {
    version: u32,
    target_revision: String,
    first_observed_unix_ms: u64,
    last_changed_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebounceDecision {
    Deferred { stable_ms: u64, next_ms: u64 },
    Due { stable_ms: u64 },
}

pub fn run_semantic_auto(
    paths: &VaultPaths,
    options: &SemanticAutoOptions,
    provider: Option<&dyn SemanticAgentProvider>,
    cancellation: &SyncCancellationToken,
    store: &SyncStateStore,
    now_unix_ms: u64,
) -> Result<SemanticAutoReport, AppError> {
    run_semantic_auto_with_reconciliation(
        paths,
        options,
        provider,
        cancellation,
        store,
        now_unix_ms,
        false,
    )
}

/// Reconciliation may check remote agreement even for locally idle/deferred
/// work. Due work always validates the remote, independently of this flag.
#[allow(clippy::too_many_arguments)]
pub fn run_semantic_auto_with_reconciliation(
    paths: &VaultPaths,
    options: &SemanticAutoOptions,
    provider: Option<&dyn SemanticAgentProvider>,
    cancellation: &SyncCancellationToken,
    store: &SyncStateStore,
    now_unix_ms: u64,
    reconcile_remote: bool,
) -> Result<SemanticAutoReport, AppError> {
    run_with_engine(
        paths,
        options,
        provider,
        cancellation,
        store,
        now_unix_ms,
        reconcile_remote,
        &crate::sync_transport::git_engine(paths),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_with_engine(
    paths: &VaultPaths,
    options: &SemanticAutoOptions,
    provider: Option<&dyn SemanticAgentProvider>,
    cancellation: &SyncCancellationToken,
    store: &SyncStateStore,
    now_unix_ms: u64,
    reconcile_remote: bool,
    engine: &dyn GitEngine,
) -> Result<SemanticAutoReport, AppError> {
    validate_options(options, provider)?;
    let vault = crate::sync_state::sync_work_tree(paths.vault_root())?;
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let source = engine
        .read_ref(&repository, &options.semantic_ref)
        .map_err(AppError::operation)?
        .ok_or_else(|| {
            AppError::operation(format!(
                "semantic branch {} does not exist",
                options.semantic_ref
            ))
        })?;
    let target = local_target(engine, &repository, options)?;
    if reconcile_remote {
        validate_remote_target(engine, &repository, options, &target)?;
    }
    let state_path = semantic_auto_state_path(store, &vault);
    if engine
        .tree_oid(&repository, &source)
        .map_err(AppError::operation)?
        == engine
            .tree_oid(&repository, &target)
            .map_err(AppError::operation)?
    {
        if !options.dry_run {
            remove_state(&state_path)?;
        }
        return Ok(base_report(
            options,
            SemanticAutoOutcome::UpToDate,
            &source,
            &target,
            0,
            None,
        ));
    }

    let prior = load_state(&state_path)?;
    let state = observed_state(prior.as_ref(), &target, now_unix_ms);
    match debounce_decision(
        &state,
        now_unix_ms,
        options.quiet_seconds,
        options.maximum_wait_seconds,
    ) {
        DebounceDecision::Deferred { stable_ms, next_ms } => {
            if !options.dry_run && prior.as_ref() != Some(&state) {
                save_state(&state_path, &state)?;
            }
            Ok(base_report(
                options,
                SemanticAutoOutcome::Deferred,
                &source,
                &target,
                stable_ms / 1_000,
                Some(next_ms),
            ))
        }
        DebounceDecision::Due { stable_ms } => {
            if !reconcile_remote {
                validate_remote_target(engine, &repository, options, &target)?;
            }
            execute_due(
                paths,
                options,
                provider,
                cancellation,
                store,
                &state_path,
                &source,
                &target,
                stable_ms,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_due(
    paths: &VaultPaths,
    options: &SemanticAutoOptions,
    provider: Option<&dyn SemanticAgentProvider>,
    cancellation: &SyncCancellationToken,
    store: &SyncStateStore,
    state_path: &Path,
    source: &GitOid,
    target: &GitOid,
    stable_ms: u64,
) -> Result<SemanticAutoReport, AppError> {
    let plan_options = SemanticPlanOptions {
        from: source.to_string(),
        to: target.to_string(),
        semantic_ref: options.semantic_ref.clone(),
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        grouping: options.grouping,
        agent: options.agent,
        dry_run: options.dry_run,
    };
    let plan = match provider {
        Some(provider) => create_semantic_plan_with_provider_and_state_store(
            paths,
            &plan_options,
            provider,
            cancellation,
            store,
        )?,
        None => create_semantic_plan_with_state_store(paths, &plan_options, store)?,
    };
    let mut report = base_report(
        options,
        if options.dry_run {
            SemanticAutoOutcome::Preview
        } else {
            SemanticAutoOutcome::Completed
        },
        source,
        target,
        stable_ms / 1_000,
        None,
    );
    report.plan = Some(plan.clone());
    if options.dry_run {
        return Ok(report);
    }
    let application = apply_semantic_plan_with_state_store(&plan.plan_id, false, store)?;
    let publication = options
        .publish
        .then(|| publish_semantic_plan_with_state_store(&plan.plan_id, false, store))
        .transpose()?;
    remove_state(state_path)?;
    report.application = Some(application);
    report.publication = publication;
    Ok(report)
}

fn local_target(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    options: &SemanticAutoOptions,
) -> Result<GitOid, AppError> {
    let refs = GitSyncRefs::for_options(&GitSyncOptions {
        remote: options.remote.clone(),
        live_ref: options.live_ref.clone(),
        ..GitSyncOptions::default()
    })
    .map_err(AppError::operation)?;
    let local = engine
        .read_ref(repository, &refs.local)
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("no accepted local live revision is available"))?;
    for reference in [&refs.fetched, &refs.pending] {
        if engine
            .read_ref(repository, reference)
            .map_err(AppError::operation)?
            .as_ref()
            != Some(&local)
        {
            return Err(AppError::operation(
                "local, fetched, and pending live refs must agree before semantic automation",
            ));
        }
    }
    Ok(local)
}

fn validate_remote_target(
    engine: &dyn GitEngine,
    repository: &vulcan_sync::GitRepository,
    options: &SemanticAutoOptions,
    local: &GitOid,
) -> Result<(), AppError> {
    if engine
        .remote_ref(repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(local)
    {
        return Err(AppError::operation(
            "remote and local accepted live refs must agree before semantic automation",
        ));
    }
    Ok(())
}

fn validate_options(
    options: &SemanticAutoOptions,
    provider: Option<&dyn SemanticAgentProvider>,
) -> Result<(), AppError> {
    if options.agent != provider.is_some() {
        return Err(AppError::operation(
            "semantic automation agent mode requires exactly one configured provider",
        ));
    }
    if options.maximum_wait_seconds == 0 {
        return Err(AppError::operation(
            "semantic automation maximum wait must be at least one second",
        ));
    }
    Ok(())
}

fn observed_state(
    prior: Option<&SemanticAutoState>,
    target: &GitOid,
    now_unix_ms: u64,
) -> SemanticAutoState {
    if let Some(prior) = prior.filter(|state| state.target_revision == target.as_str()) {
        return prior.clone();
    }
    SemanticAutoState {
        version: SEMANTIC_AUTO_VERSION,
        target_revision: target.to_string(),
        first_observed_unix_ms: prior.map_or(now_unix_ms, |state| state.first_observed_unix_ms),
        last_changed_unix_ms: now_unix_ms,
    }
}

fn debounce_decision(
    state: &SemanticAutoState,
    now_unix_ms: u64,
    quiet_seconds: u64,
    maximum_wait_seconds: u64,
) -> DebounceDecision {
    let stable_ms = now_unix_ms.saturating_sub(state.last_changed_unix_ms);
    let total_ms = now_unix_ms.saturating_sub(state.first_observed_unix_ms);
    let quiet_ms = quiet_seconds.saturating_mul(1_000);
    let maximum_ms = maximum_wait_seconds.saturating_mul(1_000);
    if stable_ms >= quiet_ms || total_ms >= maximum_ms {
        DebounceDecision::Due { stable_ms }
    } else {
        DebounceDecision::Deferred {
            stable_ms,
            next_ms: state
                .last_changed_unix_ms
                .saturating_add(quiet_ms)
                .min(state.first_observed_unix_ms.saturating_add(maximum_ms)),
        }
    }
}

fn base_report(
    options: &SemanticAutoOptions,
    outcome: SemanticAutoOutcome,
    source: &GitOid,
    target: &GitOid,
    stable_for_seconds: u64,
    next_eligible_unix_ms: Option<u64>,
) -> SemanticAutoReport {
    SemanticAutoReport {
        version: SEMANTIC_AUTO_VERSION,
        dry_run: options.dry_run,
        outcome,
        source_revision: source.to_string(),
        target_revision: target.to_string(),
        stable_for_seconds,
        next_eligible_unix_ms,
        plan: None,
        application: None,
        publication: None,
    }
}

fn semantic_auto_state_path(store: &SyncStateStore, vault: &Path) -> PathBuf {
    store
        .root()
        .join(repository_state_key(vault))
        .join("semantic-auto.json")
}

fn load_state(path: &Path) -> Result<Option<SemanticAutoState>, AppError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::operation(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_STATE_BYTES
    {
        return Err(AppError::operation(format!(
            "semantic automation state at {} is unsafe or oversized",
            path.display()
        )));
    }
    let state: SemanticAutoState =
        serde_json::from_slice(&fs::read(path).map_err(AppError::operation)?)
            .map_err(AppError::operation)?;
    if state.version != SEMANTIC_AUTO_VERSION {
        return Err(AppError::operation(format!(
            "unsupported semantic automation state version {}",
            state.version
        )));
    }
    Ok(Some(state))
}

fn save_state(path: &Path, state: &SemanticAutoState) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("semantic automation state has no parent"))?;
    fs::create_dir_all(parent).map_err(AppError::operation)?;
    let mut bytes = serde_json::to_vec_pretty(state).map_err(AppError::operation)?;
    bytes.push(b'\n');
    durable_file::replace(path, &bytes)
}

fn remove_state(path: &Path) -> Result<(), AppError> {
    durable_file::remove(path).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{debounce_decision, observed_state, DebounceDecision, SemanticAutoState};
    #[cfg(unix)]
    use vulcan_sync::GitCliEngine;
    use vulcan_sync::GitOid;

    #[test]
    fn retained_debounce_caps_continuously_changing_targets_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let first = GitOid::parse("1".repeat(40)).unwrap();
        let second = GitOid::parse("2".repeat(40)).unwrap();
        let state = observed_state(None, &first, 1_000);
        super::save_state(&path, &state).unwrap();
        let retained = super::load_state(&path).unwrap().unwrap();
        let changed = observed_state(Some(&retained), &second, 60_000);
        assert_eq!(
            debounce_decision(&changed, 61_000, 90, 60),
            DebounceDecision::Due { stable_ms: 1_000 }
        );
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::too_many_lines)] // One local-remote fixture measures idle work and both safety gates.
    fn unchanged_debounce_avoids_remote_requests_and_state_replacements() {
        use super::*;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        use std::process::Command;
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(&vault)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.invalid"]);
        fs::write(vault.join("note.md"), "# Before\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "before"]);
        let source = git(&["rev-parse", "HEAD"]);
        git(&["update-ref", "refs/heads/semantic", &source]);
        fs::write(vault.join("note.md"), "# After\n").unwrap();
        git(&["commit", "-qam", "after"]);
        let target = git(&["rev-parse", "HEAD"]);
        let refs = GitSyncRefs::for_options(&GitSyncOptions::default()).unwrap();
        for reference in [&refs.local, &refs.fetched, &refs.pending] {
            git(&["update-ref", reference.as_str(), &target]);
        }
        let remote = directory.path().join("remote.git");
        git(&["init", "--bare", "-q", remote.to_str().unwrap()]);
        git(&["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&[
            "push",
            "-q",
            "origin",
            &format!("{target}:refs/heads/__vulcan-sync/live"),
        ]);
        // Per-engine wrapper, no process-global PATH/env mutation in parallel tests.
        let wrapper = directory.path().join("git-count");
        let log = directory.path().join("commands");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec git \"$@\"\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let engine = GitCliEngine::new(&wrapper);
        let options = SemanticAutoOptions {
            semantic_ref: GitRefName::parse("refs/heads/semantic").unwrap(),
            remote: GitRemote::parse("origin").unwrap(),
            live_ref: GitRefName::parse("refs/heads/__vulcan-sync/live").unwrap(),
            grouping: SemanticGrouping::TopLevel,
            agent: false,
            publish: false,
            quiet_seconds: 900,
            maximum_wait_seconds: 3_600,
            dry_run: false,
        };
        let paths = VaultPaths::new(&vault);
        let store = SyncStateStore::at(directory.path().join("state"));
        let run = |now, reconcile| {
            run_with_engine(
                &paths,
                &options,
                None,
                &SyncCancellationToken::default(),
                &store,
                now,
                reconcile,
                &engine,
            )
        };
        assert_eq!(
            run(0, false).unwrap().outcome,
            SemanticAutoOutcome::Deferred
        );
        // Production keys state by the canonical path (macOS temp dirs sit behind a symlink).
        let state = semantic_auto_state_path(&store, &vault.canonicalize().unwrap());
        let before = fs::metadata(&state).unwrap();
        for now in (30_000..=600_000).step_by(30_000) {
            assert_eq!(
                run(now, false).unwrap().outcome,
                SemanticAutoOutcome::Deferred
            );
        }
        let after = fs::metadata(&state).unwrap();
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        let commands = fs::read_to_string(&log).unwrap();
        assert_eq!(
            commands
                .lines()
                .filter(|line| line.contains("ls-remote"))
                .count(),
            0
        );
        eprintln!("semantic: 20 unchanged deferred passes: remote requests=0 (baseline 20), state replacements=0 (baseline 20), local Git commands={}", commands.lines().count());
        git(&[
            "push",
            "-q",
            "origin",
            &format!("+{source}:refs/heads/__vulcan-sync/live"),
        ]);
        assert!(run(601_000, true)
            .unwrap_err()
            .to_string()
            .contains("remote and local"));
        assert!(run(900_000, false)
            .unwrap_err()
            .to_string()
            .contains("remote and local"));
        let commands = fs::read_to_string(&log).unwrap();
        assert_eq!(
            commands
                .lines()
                .filter(|line| line.contains("ls-remote"))
                .count(),
            2
        );
        // Local disagreement fails before making any additional remote request.
        git(&["update-ref", refs.pending.as_str(), &source]);
        assert!(run(901_000, true)
            .unwrap_err()
            .to_string()
            .contains("local, fetched, and pending"));
        assert_eq!(
            fs::read_to_string(&log)
                .unwrap()
                .lines()
                .filter(|line| line.contains("ls-remote"))
                .count(),
            2
        );
    }

    #[test]
    fn debounce_waits_for_quiet_and_resets_when_the_target_changes() {
        let first = GitOid::parse("1".repeat(40)).expect("oid");
        let second = GitOid::parse("2".repeat(40)).expect("oid");
        let state = observed_state(None, &first, 1_000);
        assert_eq!(
            debounce_decision(&state, 5_000, 10, 60),
            DebounceDecision::Deferred {
                stable_ms: 4_000,
                next_ms: 11_000
            }
        );
        let unchanged = observed_state(Some(&state), &first, 6_000);
        assert_eq!(unchanged, state);
        let changed = observed_state(Some(&state), &second, 6_000);
        assert_eq!(changed.first_observed_unix_ms, 1_000);
        assert_eq!(changed.last_changed_unix_ms, 6_000);
    }

    #[test]
    fn debounce_runs_at_quiet_or_maximum_deadline() {
        let state = SemanticAutoState {
            version: 1,
            target_revision: "1".repeat(40),
            first_observed_unix_ms: 1_000,
            last_changed_unix_ms: 10_000,
        };
        assert_eq!(
            debounce_decision(&state, 20_000, 10, 60),
            DebounceDecision::Due { stable_ms: 10_000 }
        );
        let state = SemanticAutoState {
            first_observed_unix_ms: 1_000,
            last_changed_unix_ms: 55_000,
            ..state
        };
        assert_eq!(
            debounce_decision(&state, 61_000, 30, 60),
            DebounceDecision::Due { stable_ms: 6_000 }
        );
    }
}
