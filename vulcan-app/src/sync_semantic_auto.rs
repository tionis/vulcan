//! Debounced, finite semantic-history automation for daemons and CI schedulers.

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
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use vulcan_core::VaultPaths;
use vulcan_sync::{GitCliEngine, GitEngine, GitOid, GitSyncOptions, GitSyncRefs};

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
    validate_options(options, provider)?;
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = GitCliEngine::default();
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
    let target = accepted_target(&engine, &repository, options)?;
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
            if !options.dry_run {
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
        DebounceDecision::Due { stable_ms } => execute_due(
            paths,
            options,
            provider,
            cancellation,
            store,
            &state_path,
            &source,
            &target,
            stable_ms,
        ),
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

fn accepted_target(
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
    if engine
        .remote_ref(repository, &options.remote, &options.live_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(&local)
    {
        return Err(AppError::operation(
            "remote and local accepted live refs must agree before semantic automation",
        ));
    }
    Ok(local)
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
        first_observed_unix_ms: now_unix_ms,
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
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary
        .write_all(&serde_json::to_vec_pretty(state).map_err(AppError::operation)?)
        .map_err(AppError::operation)?;
    temporary.write_all(b"\n").map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    temporary
        .persist(path)
        .map_err(|error| AppError::operation(error.error))?;
    Ok(())
}

fn remove_state(path: &Path) -> Result<(), AppError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::operation(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::{debounce_decision, observed_state, DebounceDecision, SemanticAutoState};
    use vulcan_sync::GitOid;

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
        assert_eq!(changed.first_observed_unix_ms, 6_000);
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
