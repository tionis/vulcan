//! Reviewable semantic histories derived from immutable accepted sync snapshots.

use crate::sync_state::{repository_state_key, SyncStateStore};
use crate::AppError;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use ulid::Ulid;
use vulcan_core::VaultPaths;
use vulcan_sync::{
    GitCliEngine, GitEngine, GitOid, GitRefName, GitRefUpdateResult, GitRemote, GitRepository,
    GitSyncOptions, GitSyncRefs,
};

pub const SEMANTIC_PLAN_VERSION: u32 = 1;
const MAX_SEMANTIC_PLAN_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticPlanStatus {
    Preview,
    Prepared,
    Ready,
    Applying,
    Applied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticPlanOptions {
    pub from: String,
    pub to: String,
    pub semantic_ref: GitRefName,
    pub remote: GitRemote,
    pub live_ref: GitRefName,
    pub agent: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticCommitProposal {
    pub position: usize,
    pub group: String,
    pub message: String,
    pub paths: Vec<String>,
    pub from_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct SemanticPlanValidation {
    pub source_ref_matches: bool,
    pub source_is_ancestor: bool,
    pub target_is_accepted_live: bool,
    pub final_tree_matches_target: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticPlanReport {
    pub version: u32,
    pub plan_id: String,
    pub status: SemanticPlanStatus,
    pub dry_run: bool,
    pub agent: bool,
    pub vault: PathBuf,
    pub repository_key: String,
    pub semantic_ref: String,
    pub proposal_ref: String,
    pub remote: String,
    pub live_ref: String,
    pub source_revision: String,
    pub target_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_tip: Option<String>,
    pub commits: Vec<SemanticCommitProposal>,
    pub validation: SemanticPlanValidation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticApplyReport {
    pub version: u32,
    pub plan_id: String,
    pub dry_run: bool,
    pub semantic_ref: String,
    pub previous_revision: String,
    pub applied_revision: String,
    pub target_revision: String,
}

pub fn create_semantic_plan(
    paths: &VaultPaths,
    options: &SemanticPlanOptions,
) -> Result<SemanticPlanReport, AppError> {
    if options.agent {
        return Err(AppError::operation(
            "agent-assisted semantic grouping is not available yet; omit --agent to use the deterministic grouper",
        ));
    }
    let store = SyncStateStore::user_default()?;
    create_semantic_plan_with_state_store(paths, options, &store)
}

pub fn create_semantic_plan_with_state_store(
    paths: &VaultPaths,
    options: &SemanticPlanOptions,
    store: &SyncStateStore,
) -> Result<SemanticPlanReport, AppError> {
    let vault = fs::canonicalize(paths.vault_root()).map_err(AppError::operation)?;
    let engine = GitCliEngine::default();
    let repository = engine
        .discover_repository(&vault)
        .map_err(AppError::operation)?;
    let source = engine
        .resolve_revision(&repository, &options.from)
        .map_err(AppError::operation)?;
    let target = engine
        .resolve_revision(&repository, &options.to)
        .map_err(AppError::operation)?;
    validate_plan_inputs(&engine, &repository, options, &source, &target)?;
    let plan_id = Ulid::new().to_string().to_ascii_lowercase();
    let proposal_ref = semantic_proposal_ref(&plan_id)?;
    let groups = group_changed_paths(
        engine
            .changed_paths(&repository, &source, &target)
            .map_err(AppError::operation)?,
    );
    let mut report =
        initial_plan_report(&vault, options, &source, &target, &plan_id, &proposal_ref);
    if options.dry_run {
        report.commits = preview_commits(&engine, &repository, &source, &target, groups)?;
        report.validation.final_tree_matches_target = true;
        return Ok(report);
    }

    let _lock = SemanticLock::acquire(&repository)?;
    validate_plan_inputs(&engine, &repository, options, &source, &target)?;
    report.status = SemanticPlanStatus::Prepared;
    save_plan(store, &report, true)?;
    let (commits, tip) =
        construct_proposal(&engine, &repository, &source, &target, &plan_id, groups)?;
    let final_tree = engine
        .tree_oid(&repository, &tip)
        .map_err(AppError::operation)?;
    let target_tree = engine
        .tree_oid(&repository, &target)
        .map_err(AppError::operation)?;
    if final_tree != target_tree {
        return Err(AppError::operation(format!(
            "semantic proposal final tree {final_tree} differs from accepted target tree {target_tree}"
        )));
    }
    if engine
        .compare_and_swap_ref(&repository, &proposal_ref, &tip, None)
        .map_err(AppError::operation)?
        != GitRefUpdateResult::Updated
    {
        return Err(AppError::operation(format!(
            "semantic proposal ref {proposal_ref} already exists"
        )));
    }
    report.status = SemanticPlanStatus::Ready;
    report.proposal_tip = Some(tip.to_string());
    report.commits = commits;
    report.validation.final_tree_matches_target = true;
    save_plan(store, &report, false)?;
    Ok(report)
}

pub fn load_semantic_plan(plan_id: &str) -> Result<SemanticPlanReport, AppError> {
    load_semantic_plan_with_state_store(plan_id, &SyncStateStore::user_default()?)
}

pub fn load_semantic_plan_with_state_store(
    plan_id: &str,
    store: &SyncStateStore,
) -> Result<SemanticPlanReport, AppError> {
    validate_plan_id(plan_id)?;
    let path = semantic_plan_path(store, plan_id);
    let metadata = fs::metadata(&path).map_err(AppError::operation)?;
    if metadata.len() > MAX_SEMANTIC_PLAN_BYTES {
        return Err(AppError::operation(format!(
            "semantic plan at {} exceeds the {} byte limit",
            path.display(),
            MAX_SEMANTIC_PLAN_BYTES
        )));
    }
    let plan: SemanticPlanReport =
        serde_json::from_slice(&fs::read(&path).map_err(AppError::operation)?)
            .map_err(AppError::operation)?;
    validate_loaded_plan(plan_id, &path, &plan)?;
    Ok(plan)
}

pub fn apply_semantic_plan(plan_id: &str, dry_run: bool) -> Result<SemanticApplyReport, AppError> {
    let store = SyncStateStore::user_default()?;
    apply_semantic_plan_with_state_store(plan_id, dry_run, &store)
}

pub fn apply_semantic_plan_with_state_store(
    plan_id: &str,
    dry_run: bool,
    store: &SyncStateStore,
) -> Result<SemanticApplyReport, AppError> {
    let mut plan = load_semantic_plan_with_state_store(plan_id, store)?;
    if !matches!(
        plan.status,
        SemanticPlanStatus::Ready | SemanticPlanStatus::Applying | SemanticPlanStatus::Applied
    ) {
        return Err(AppError::operation(format!(
            "semantic plan {plan_id} is not ready for application"
        )));
    }
    let engine = GitCliEngine::default();
    let repository = engine
        .discover_repository(&plan.vault)
        .map_err(AppError::operation)?;
    if repository_state_key(&plan.vault) != plan.repository_key {
        return Err(AppError::operation(
            "semantic plan vault identity no longer matches its repository key",
        ));
    }
    let _lock = SemanticLock::acquire(&repository)?;
    let source = GitOid::parse(plan.source_revision.clone()).map_err(AppError::operation)?;
    let target = GitOid::parse(plan.target_revision.clone()).map_err(AppError::operation)?;
    let tip = GitOid::parse(
        plan.proposal_tip
            .clone()
            .ok_or_else(|| AppError::operation("semantic plan has no proposal tip"))?,
    )
    .map_err(AppError::operation)?;
    let semantic_ref = GitRefName::parse(plan.semantic_ref.clone()).map_err(AppError::operation)?;
    let proposal_ref = GitRefName::parse(plan.proposal_ref.clone()).map_err(AppError::operation)?;
    let remote = GitRemote::parse(plan.remote.clone()).map_err(AppError::operation)?;
    let live_ref = GitRefName::parse(plan.live_ref.clone()).map_err(AppError::operation)?;
    if matches!(
        plan.status,
        SemanticPlanStatus::Applying | SemanticPlanStatus::Applied
    ) && engine
        .read_ref(&repository, &semantic_ref)
        .map_err(AppError::operation)?
        .as_ref()
        == Some(&tip)
    {
        validate_applied_inputs(
            &engine,
            &repository,
            &proposal_ref,
            &remote,
            &live_ref,
            &target,
            &tip,
        )?;
        let report = semantic_apply_report(plan_id, dry_run, &semantic_ref, &source, &tip, &target);
        if !dry_run && plan.status != SemanticPlanStatus::Applied {
            plan.status = SemanticPlanStatus::Applied;
            save_plan(store, &plan, false)?;
        }
        return Ok(report);
    }
    validate_apply_inputs(
        &engine,
        &repository,
        &semantic_ref,
        &proposal_ref,
        &remote,
        &live_ref,
        &source,
        &target,
        &tip,
    )?;
    let report = semantic_apply_report(plan_id, dry_run, &semantic_ref, &source, &tip, &target);
    if dry_run {
        return Ok(report);
    }
    plan.status = SemanticPlanStatus::Applying;
    save_plan(store, &plan, false)?;
    if engine
        .compare_and_swap_ref(&repository, &semantic_ref, &tip, Some(&source))
        .map_err(AppError::operation)?
        != GitRefUpdateResult::Updated
    {
        return Err(AppError::operation(
            "semantic branch changed while applying the proposal; the plan is stale",
        ));
    }
    plan.status = SemanticPlanStatus::Applied;
    save_plan(store, &plan, false)?;
    Ok(report)
}

fn validate_plan_inputs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    options: &SemanticPlanOptions,
    source: &GitOid,
    target: &GitOid,
) -> Result<(), AppError> {
    if engine
        .read_ref(repository, &options.semantic_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(source)
    {
        return Err(AppError::operation(format!(
            "semantic ref {} does not identify the selected source revision {source}",
            options.semantic_ref
        )));
    }
    if !engine
        .is_ancestor(repository, source, target)
        .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "semantic source must be an ancestor of the accepted target",
        ));
    }
    validate_accepted_target(
        engine,
        repository,
        &options.remote,
        &options.live_ref,
        target,
    )
}

fn validate_accepted_target(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    remote: &GitRemote,
    live_ref: &GitRefName,
    target: &GitOid,
) -> Result<(), AppError> {
    let refs = GitSyncRefs::for_options(&GitSyncOptions {
        remote: remote.clone(),
        live_ref: live_ref.clone(),
        ..GitSyncOptions::default()
    })
    .map_err(AppError::operation)?;
    for (name, reference) in [
        ("local", &refs.local),
        ("fetched", &refs.fetched),
        ("pending", &refs.pending),
    ] {
        if engine
            .read_ref(repository, reference)
            .map_err(AppError::operation)?
            .as_ref()
            != Some(target)
        {
            return Err(AppError::operation(format!(
                "the {name} sync ref does not identify the selected accepted target {target}"
            )));
        }
    }
    if engine
        .remote_ref(repository, remote, live_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(target)
    {
        return Err(AppError::operation(
            "the remote live ref does not identify the selected accepted target",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_apply_inputs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    semantic_ref: &GitRefName,
    proposal_ref: &GitRefName,
    remote: &GitRemote,
    live_ref: &GitRefName,
    source: &GitOid,
    target: &GitOid,
    tip: &GitOid,
) -> Result<(), AppError> {
    if engine
        .read_ref(repository, semantic_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(source)
    {
        return Err(AppError::operation(
            "semantic source branch moved after the plan was created",
        ));
    }
    let safety = engine
        .safety_state(repository)
        .map_err(AppError::operation)?;
    if safety.staged_changes || safety.operation.is_some() {
        return Err(AppError::operation(
            "cannot apply a semantic plan while the normal Git index is staged or an operation is in progress",
        ));
    }
    if engine
        .read_ref(repository, proposal_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(tip)
    {
        return Err(AppError::operation(
            "semantic proposal ref no longer identifies the recorded proposal tip",
        ));
    }
    if !engine
        .is_ancestor(repository, source, tip)
        .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "semantic proposal is not a fast-forward of its source",
        ));
    }
    if engine
        .tree_oid(repository, tip)
        .map_err(AppError::operation)?
        != engine
            .tree_oid(repository, target)
            .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "semantic proposal final tree no longer matches the selected live target",
        ));
    }
    validate_accepted_target(engine, repository, remote, live_ref, target)
}

fn validate_applied_inputs(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    proposal_ref: &GitRefName,
    remote: &GitRemote,
    live_ref: &GitRefName,
    target: &GitOid,
    tip: &GitOid,
) -> Result<(), AppError> {
    if engine
        .read_ref(repository, proposal_ref)
        .map_err(AppError::operation)?
        .as_ref()
        != Some(tip)
    {
        return Err(AppError::operation(
            "semantic proposal ref no longer identifies the applied proposal tip",
        ));
    }
    if engine
        .tree_oid(repository, tip)
        .map_err(AppError::operation)?
        != engine
            .tree_oid(repository, target)
            .map_err(AppError::operation)?
    {
        return Err(AppError::operation(
            "applied semantic proposal no longer matches the selected live target tree",
        ));
    }
    validate_accepted_target(engine, repository, remote, live_ref, target)
}

fn semantic_apply_report(
    plan_id: &str,
    dry_run: bool,
    semantic_ref: &GitRefName,
    source: &GitOid,
    tip: &GitOid,
    target: &GitOid,
) -> SemanticApplyReport {
    SemanticApplyReport {
        version: SEMANTIC_PLAN_VERSION,
        plan_id: plan_id.to_string(),
        dry_run,
        semantic_ref: semantic_ref.to_string(),
        previous_revision: source.to_string(),
        applied_revision: tip.to_string(),
        target_revision: target.to_string(),
    }
}

fn construct_proposal(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    source: &GitOid,
    target: &GitOid,
    plan_id: &str,
    groups: BTreeMap<String, Vec<String>>,
) -> Result<(Vec<SemanticCommitProposal>, GitOid), AppError> {
    let mut parent = source.clone();
    let mut commits = Vec::with_capacity(groups.len());
    for (position, (group, paths)) in groups.into_iter().enumerate() {
        let tree = engine
            .tree_with_paths(repository, &parent, target, &paths)
            .map_err(AppError::operation)?;
        let message = semantic_message(&group, plan_id, source, target);
        let commit = engine
            .create_commit(repository, &tree, std::slice::from_ref(&parent), &message)
            .map_err(AppError::operation)?;
        validate_intermediate_commit(engine, repository, &parent, &commit, target, &paths)?;
        let patch = engine
            .diff_patch(repository, &parent, &commit, &paths)
            .map_err(AppError::operation)?;
        commits.push(SemanticCommitProposal {
            position: position + 1,
            group,
            message,
            paths,
            from_revision: parent.to_string(),
            revision: Some(commit.to_string()),
            tree: Some(tree.to_string()),
            patch,
        });
        parent = commit;
    }
    Ok((commits, parent))
}

fn validate_intermediate_commit(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    parent: &GitOid,
    commit: &GitOid,
    target: &GitOid,
    paths: &[String],
) -> Result<(), AppError> {
    let mut actual = engine
        .changed_paths(repository, parent, commit)
        .map_err(AppError::operation)?;
    let mut expected = paths.to_vec();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(AppError::operation(format!(
            "semantic intermediate commit {commit} changed paths outside its proposed group"
        )));
    }
    for path in paths {
        let proposed = engine
            .path_object(repository, commit, path)
            .map_err(AppError::operation)?;
        let accepted = engine
            .path_object(repository, target, path)
            .map_err(AppError::operation)?;
        let same = match (&proposed, &accepted) {
            (None, None) => true,
            (Some(proposed), Some(accepted)) => {
                proposed.oid == accepted.oid
                    && proposed.mode == accepted.mode
                    && proposed.kind == accepted.kind
            }
            _ => false,
        };
        if !same {
            return Err(AppError::operation(format!(
                "semantic intermediate commit {commit} does not reproduce accepted path {path}"
            )));
        }
    }
    Ok(())
}

fn preview_commits(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    source: &GitOid,
    target: &GitOid,
    groups: BTreeMap<String, Vec<String>>,
) -> Result<Vec<SemanticCommitProposal>, AppError> {
    groups
        .into_iter()
        .enumerate()
        .map(|(position, (group, paths))| {
            let patch = engine
                .diff_patch(repository, source, target, &paths)
                .map_err(AppError::operation)?;
            Ok(SemanticCommitProposal {
                position: position + 1,
                message: semantic_message(&group, "dry-run", source, target),
                group,
                paths,
                from_revision: source.to_string(),
                revision: None,
                tree: None,
                patch,
            })
        })
        .collect()
}

fn group_changed_paths(paths: Vec<String>) -> BTreeMap<String, Vec<String>> {
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for path in paths {
        let group = path
            .split_once('/')
            .map_or_else(|| path.clone(), |(top, _)| top.to_string());
        groups.entry(group).or_default().push(path);
    }
    groups
}

fn semantic_message(group: &str, plan_id: &str, source: &GitOid, target: &GitOid) -> String {
    format!(
        "Update {group}\n\nVulcan-Semantic-Version: 1\nVulcan-Semantic-Plan: {plan_id}\nVulcan-Semantic-Source: {source}\nVulcan-Semantic-Target: {target}\nVulcan-Semantic-Group: {group}\n"
    )
}

fn initial_plan_report(
    vault: &Path,
    options: &SemanticPlanOptions,
    source: &GitOid,
    target: &GitOid,
    plan_id: &str,
    proposal_ref: &GitRefName,
) -> SemanticPlanReport {
    SemanticPlanReport {
        version: SEMANTIC_PLAN_VERSION,
        plan_id: plan_id.to_string(),
        status: SemanticPlanStatus::Preview,
        dry_run: options.dry_run,
        agent: options.agent,
        vault: vault.to_path_buf(),
        repository_key: repository_state_key(vault),
        semantic_ref: options.semantic_ref.to_string(),
        proposal_ref: proposal_ref.to_string(),
        remote: options.remote.to_string(),
        live_ref: options.live_ref.to_string(),
        source_revision: source.to_string(),
        target_revision: target.to_string(),
        proposal_tip: None,
        commits: Vec::new(),
        validation: SemanticPlanValidation {
            source_ref_matches: true,
            source_is_ancestor: true,
            target_is_accepted_live: true,
            final_tree_matches_target: false,
        },
    }
}

fn semantic_proposal_ref(plan_id: &str) -> Result<GitRefName, AppError> {
    GitRefName::parse(format!("refs/vulcan/proposals/semantic/{plan_id}"))
        .map_err(AppError::operation)
}

fn semantic_plan_path(store: &SyncStateStore, plan_id: &str) -> PathBuf {
    store
        .root()
        .join("_semantic_plans")
        .join(format!("{plan_id}.json"))
}

fn save_plan(
    store: &SyncStateStore,
    plan: &SemanticPlanReport,
    create: bool,
) -> Result<(), AppError> {
    validate_plan_id(&plan.plan_id)?;
    let path = semantic_plan_path(store, &plan.plan_id);
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("semantic plan path has no parent"))?;
    fs::create_dir_all(parent).map_err(AppError::operation)?;
    let bytes = serde_json::to_vec_pretty(plan).map_err(AppError::operation)?;
    if bytes.len() as u64 > MAX_SEMANTIC_PLAN_BYTES {
        return Err(AppError::operation(format!(
            "semantic plan exceeds the {MAX_SEMANTIC_PLAN_BYTES} byte limit"
        )));
    }
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    temporary.write_all(&bytes).map_err(AppError::operation)?;
    temporary.write_all(b"\n").map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    if create {
        temporary
            .persist_noclobber(path)
            .map_err(|error| AppError::operation(error.error))?;
    } else {
        temporary
            .persist(path)
            .map_err(|error| AppError::operation(error.error))?;
    }
    Ok(())
}

fn validate_loaded_plan(
    plan_id: &str,
    path: &Path,
    plan: &SemanticPlanReport,
) -> Result<(), AppError> {
    if plan.version != SEMANTIC_PLAN_VERSION {
        return Err(AppError::operation(format!(
            "unsupported semantic plan version {} at {}",
            plan.version,
            path.display()
        )));
    }
    if plan.plan_id != plan_id {
        return Err(AppError::operation(format!(
            "semantic plan identity mismatch at {}",
            path.display()
        )));
    }
    if semantic_proposal_ref(plan_id)?.as_str() != plan.proposal_ref {
        return Err(AppError::operation(format!(
            "semantic proposal ref mismatch at {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_plan_id(plan_id: &str) -> Result<(), AppError> {
    let parsed = Ulid::from_string(&plan_id.to_ascii_uppercase())
        .map_err(|_| AppError::operation("semantic plan ID must be a 26-character ULID"))?;
    if parsed.to_string().to_ascii_lowercase() != plan_id {
        return Err(AppError::operation(
            "semantic plan ID must use canonical lowercase Crockford Base32",
        ));
    }
    Ok(())
}

struct SemanticLock {
    _file: File,
}

impl SemanticLock {
    fn acquire(repository: &GitRepository) -> Result<Self, AppError> {
        let path = repository.git_dir.join("vulcan-sync/sync.lock");
        fs::create_dir_all(
            path.parent()
                .expect("the sync repository lock always has a parent"),
        )
        .map_err(AppError::operation)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(AppError::operation)?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                AppError::operation("another synchronization operation holds the repository lock")
            } else {
                AppError::operation(error)
            }
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::{group_changed_paths, semantic_proposal_ref, validate_plan_id};

    #[test]
    fn deterministic_groups_are_top_level_and_sorted() {
        let groups = group_changed_paths(vec![
            "Z.md".to_string(),
            "Area/Two.md".to_string(),
            "Area/One.md".to_string(),
            "A.md".to_string(),
        ]);
        assert_eq!(
            groups.keys().cloned().collect::<Vec<_>>(),
            ["A.md", "Area", "Z.md"]
        );
        assert_eq!(groups["Area"], ["Area/Two.md", "Area/One.md"]);
    }

    #[test]
    fn semantic_plan_ids_are_canonical_and_ref_safe() {
        let id = "01arz3ndektsv4rrffq69g5fav";
        validate_plan_id(id).expect("canonical plan ID");
        assert_eq!(
            semantic_proposal_ref(id).expect("proposal ref").as_str(),
            "refs/vulcan/proposals/semantic/01arz3ndektsv4rrffq69g5fav"
        );
        assert!(validate_plan_id("01ARZ3NDEKTSV4RRFFQ69G5FAV").is_err());
        assert!(validate_plan_id("../unsafe").is_err());
    }
}
