//! Batched auto-commits: one finite pass that commits the changes a vault
//! accumulated since its last batch (`[git] strategy = "batched"`).
//!
//! The daemon runs a pass on its interval; the pass itself is synchronous and
//! daemon-independent. It never stages or commits paths someone else staged,
//! waits out merges and rebases, and stands down when Git sync owns capture.
//! An agent may group and describe the changes but never changes their
//! bytes; when it cannot, the batch is committed with deterministic groups.

use crate::commit::{capture_owner, CaptureOwner};
use crate::execution::MutationGate;
use crate::sync::SyncCancellationToken;
use crate::sync_semantic::{
    validate_semantic_agent_commits, SemanticAgentChange, SemanticAgentIdentity,
    SemanticAgentProvider, SemanticAgentRequest,
};
use crate::AppError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use vulcan_core::{
    batch_commit_candidates, batch_commit_patches, commit_batch_groups, is_git_repo,
    load_vault_config, render_auto_commit_message, resolve_permission_profile,
    BatchCommitCandidates, BatchCommitGroup, BatchCommitOutcome, BatchCommitRecord,
    GitBatchMessages, GitCommitStrategy, GitConfig, PermissionGuard, PluginEvent,
    ProfilePermissionGuard, VaultPaths, Verbosity,
};

pub const BATCHED_COMMIT_REPORT_VERSION: u32 = 1;
/// The shortest and longest batch intervals; configured values are clamped.
pub const MIN_BATCH_INTERVAL_SECONDS: u64 = 30;
pub const MAX_BATCH_INTERVAL_SECONDS: u64 = 24 * 60 * 60;
/// Larger batches are grouped deterministically instead of by the agent.
const MAX_AGENT_PATHS: usize = 200;
const MAX_AGENT_PATCH_BYTES: usize = 1024 * 1024;
const BATCH_ACTION: &str = "batch";

/// The batch interval `config` asks for, within the supported bounds.
#[must_use]
pub fn effective_batch_interval_seconds(config: &GitConfig) -> u64 {
    config
        .batch_interval_seconds
        .clamp(MIN_BATCH_INTERVAL_SECONDS, MAX_BATCH_INTERVAL_SECONDS)
}

/// Whether `config` asks for batched commits at all.
#[must_use]
pub fn batched_commits_enabled(config: &GitConfig) -> bool {
    config.auto_commit && config.strategy == GitCommitStrategy::Batched
}

pub struct BatchedCommitOptions<'a> {
    /// The device's semantic agent, when one is configured and this vault may
    /// use it. `agent_unavailable` says why not otherwise.
    pub agent: Option<&'a dyn SemanticAgentProvider>,
    pub agent_unavailable: Option<String>,
    /// Who owns capture, when the caller already knows (the daemon reads it
    /// from the registration). `None` reads the device registry.
    pub capture_owner: Option<CaptureOwner>,
    pub permission_profile: Option<&'a str>,
    pub gate: &'a dyn MutationGate,
    pub cancellation: &'a SyncCancellationToken,
    pub verbosity: Verbosity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchedCommitStatus {
    /// The batch was committed.
    Committed,
    /// Nothing outside the exclusions and manual staging had changed.
    Clean,
    /// The repository is mid-operation or not committable now.
    Blocked,
    /// The vault does not ask for batched commits.
    Disabled,
    /// Git sync owns automatic capture for this vault.
    OwnedBySync,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchedCommitReport {
    pub version: u32,
    pub status: BatchedCommitStatus,
    pub requested_messages: GitBatchMessages,
    /// How the committed groups were made; differs from the request after a
    /// fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_messages: Option<GitBatchMessages>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<SemanticAgentIdentity>,
    /// Why the agent's grouping was not used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<BatchCommitRecord>,
    /// Paths someone staged by hand, which the batch left alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_staged: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl BatchedCommitReport {
    fn new(status: BatchedCommitStatus, requested: GitBatchMessages) -> Self {
        Self {
            version: BATCHED_COMMIT_REPORT_VERSION,
            status,
            requested_messages: requested,
            used_messages: None,
            agent: None,
            fallback: None,
            commits: Vec::new(),
            left_staged: Vec::new(),
            detail: None,
        }
    }

    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Commits the vault's accumulated changes once, as `[git]` configures.
pub fn run_batched_commit(
    paths: &VaultPaths,
    options: &BatchedCommitOptions<'_>,
) -> Result<BatchedCommitReport, AppError> {
    let config = load_vault_config(paths).config.git;
    let requested = config.batch_messages;
    if let Some(report) = batch_precondition(paths, &config, options) {
        return Ok(report);
    }
    let guard = ProfilePermissionGuard::new(
        paths,
        resolve_permission_profile(paths, options.permission_profile)
            .map_err(AppError::operation)?,
    );
    guard.check_git().map_err(AppError::operation)?;

    let mut candidates =
        batch_commit_candidates(paths.vault_root(), &config).map_err(AppError::operation)?;
    let mut plan = plan_batch(paths, &config, &candidates, &guard, options)?;
    // A plan made against an older work tree is redone deterministically once:
    // the agent is not asked again while a burst of edits is still landing.
    for attempt in 0..2 {
        let mut report = BatchedCommitReport::new(BatchedCommitStatus::Clean, requested);
        report.left_staged.clone_from(&candidates.left_staged);
        if let Some(reason) = &candidates.blocked {
            report.status = BatchedCommitStatus::Blocked;
            report.detail = Some(reason.clone());
            return Ok(report);
        }
        if candidates.paths.is_empty() {
            return Ok(report);
        }
        cancellation_check(options.cancellation)?;
        crate::plugins::dispatch_plugin_event(
            paths,
            options.permission_profile,
            PluginEvent::OnPreCommit,
            &json!({
                "kind": PluginEvent::OnPreCommit,
                "action": BATCH_ACTION,
                "files": candidates.paths,
            }),
            options.verbosity,
        )?;
        let outcome = {
            let _permit = options.gate.enter()?;
            commit_batch_groups(paths.vault_root(), &config, &candidates, &plan.groups)
                .map_err(AppError::operation)?
        };
        match outcome {
            BatchCommitOutcome::Committed { commits } => {
                announce_commits(paths, options, &commits);
                report.status = BatchedCommitStatus::Committed;
                report.used_messages = Some(plan.used);
                report.agent = plan.agent;
                report.fallback = plan.fallback;
                report.commits = commits;
                return Ok(report);
            }
            BatchCommitOutcome::Blocked { reason } => {
                report.status = BatchedCommitStatus::Blocked;
                report.detail = Some(reason);
                return Ok(report);
            }
            BatchCommitOutcome::Changed { current } => {
                if attempt == 1 {
                    report.status = BatchedCommitStatus::Blocked;
                    report.detail =
                        Some("the work tree kept changing while the batch was committed".into());
                    return Ok(report);
                }
                let fallback = (requested == GitBatchMessages::Agent)
                    .then(|| "the work tree changed after the agent planned the batch".into());
                candidates = current;
                let used = if requested == GitBatchMessages::Template {
                    GitBatchMessages::Template
                } else {
                    GitBatchMessages::Deterministic
                };
                plan = BatchPlan {
                    groups: deterministic_or_template(&config, &candidates, used),
                    used,
                    agent: None,
                    fallback,
                };
            }
        }
    }
    unreachable!("the batch loop returns on its second attempt")
}

fn announce_commits(
    paths: &VaultPaths,
    options: &BatchedCommitOptions<'_>,
    commits: &[BatchCommitRecord],
) {
    for commit in commits {
        let _ = crate::plugins::dispatch_plugin_event(
            paths,
            options.permission_profile,
            PluginEvent::OnPostCommit,
            &json!({
                "kind": PluginEvent::OnPostCommit,
                "action": BATCH_ACTION,
                "files": commit.files,
                "sha": commit.sha,
                "message": commit.message,
            }),
            options.verbosity,
        );
    }
}

/// The report for a vault whose configuration, capture owner, or repository
/// rules out a batch, or `None` when a batch may run.
fn batch_precondition(
    paths: &VaultPaths,
    config: &GitConfig,
    options: &BatchedCommitOptions<'_>,
) -> Option<BatchedCommitReport> {
    let requested = config.batch_messages;
    if !batched_commits_enabled(config) {
        return Some(BatchedCommitReport::new(
            BatchedCommitStatus::Disabled,
            requested,
        ));
    }
    let owner = options
        .capture_owner
        .clone()
        .unwrap_or_else(|| capture_owner(paths.vault_root()));
    if let CaptureOwner::HiddenRefSync { wiki_id } = owner {
        return Some(
            BatchedCommitReport::new(BatchedCommitStatus::OwnedBySync, requested).with_detail(
                format!("Git sync for `{wiki_id}` captures this vault's changes"),
            ),
        );
    }
    if !is_git_repo(paths.vault_root()) {
        return Some(
            BatchedCommitReport::new(BatchedCommitStatus::Blocked, requested)
                .with_detail("the vault is not a git repository"),
        );
    }
    None
}

struct BatchPlan {
    groups: Vec<BatchCommitGroup>,
    used: GitBatchMessages,
    agent: Option<SemanticAgentIdentity>,
    fallback: Option<String>,
}

fn plan_batch(
    paths: &VaultPaths,
    config: &GitConfig,
    candidates: &BatchCommitCandidates,
    guard: &ProfilePermissionGuard,
    options: &BatchedCommitOptions<'_>,
) -> Result<BatchPlan, AppError> {
    if candidates.paths.is_empty() || candidates.blocked.is_some() {
        return Ok(BatchPlan {
            groups: Vec::new(),
            used: config.batch_messages,
            agent: None,
            fallback: None,
        });
    }
    if config.batch_messages != GitBatchMessages::Agent {
        return Ok(BatchPlan {
            groups: deterministic_or_template(config, candidates, config.batch_messages),
            used: config.batch_messages,
            agent: None,
            fallback: None,
        });
    }
    let (agent, fallback) = match agent_groups(paths, candidates, guard, options) {
        Ok((groups, identity)) => {
            return Ok(BatchPlan {
                groups,
                used: GitBatchMessages::Agent,
                agent: Some(identity),
                fallback: None,
            });
        }
        Err(AgentFallback { identity, reason }) => (identity, reason),
    };
    cancellation_check(options.cancellation)?;
    Ok(BatchPlan {
        groups: deterministic_or_template(config, candidates, GitBatchMessages::Deterministic),
        used: GitBatchMessages::Deterministic,
        agent,
        fallback: Some(fallback),
    })
}

struct AgentFallback {
    identity: Option<SemanticAgentIdentity>,
    reason: String,
}

impl AgentFallback {
    fn new(identity: Option<SemanticAgentIdentity>, reason: impl Into<String>) -> Self {
        Self {
            identity,
            reason: reason.into(),
        }
    }
}

fn agent_groups(
    paths: &VaultPaths,
    candidates: &BatchCommitCandidates,
    guard: &ProfilePermissionGuard,
    options: &BatchedCommitOptions<'_>,
) -> Result<(Vec<BatchCommitGroup>, SemanticAgentIdentity), AgentFallback> {
    let Some(agent) = options.agent else {
        return Err(AgentFallback::new(
            None,
            options
                .agent_unavailable
                .clone()
                .unwrap_or_else(|| "no semantic agent is configured on this device".to_string()),
        ));
    };
    let identity = agent.identity();
    if let Some(endpoint) = agent.network_endpoint() {
        if let Err(error) = guard.check_network(endpoint) {
            return Err(AgentFallback::new(
                Some(identity),
                format!("the permission profile denies the semantic agent: {error}"),
            ));
        }
    }
    if candidates.paths.len() > MAX_AGENT_PATHS {
        return Err(AgentFallback::new(
            Some(identity),
            format!(
                "the batch changes {} paths, more than the agent limit of {MAX_AGENT_PATHS}",
                candidates.paths.len()
            ),
        ));
    }
    let patches = match batch_commit_patches(paths.vault_root(), candidates, MAX_AGENT_PATCH_BYTES)
    {
        Ok(Some(patches)) => patches,
        Ok(None) => {
            return Err(AgentFallback::new(
                Some(identity),
                format!(
                    "the batch's patches exceed the agent limit of {MAX_AGENT_PATCH_BYTES} bytes"
                ),
            ));
        }
        Err(error) => {
            return Err(AgentFallback::new(
                Some(identity),
                format!("cannot prepare the batch's patches: {error}"),
            ));
        }
    };
    let request = SemanticAgentRequest {
        source_revision: candidates
            .head
            .clone()
            .unwrap_or_else(|| "unborn".to_string()),
        target_revision: "working-tree".to_string(),
        changes: patches
            .into_iter()
            .map(|(path, patch)| SemanticAgentChange { path, patch })
            .collect(),
    };
    let output = agent
        .propose(&request, options.cancellation)
        .map_err(|error| AgentFallback::new(Some(identity.clone()), error.to_string()))?;
    let commits = validate_semantic_agent_commits(&identity, &candidates.paths, output)
        .map_err(|error| AgentFallback::new(Some(identity.clone()), error.to_string()))?;
    Ok((
        commits
            .into_iter()
            .map(|commit| BatchCommitGroup {
                message: commit.message,
                paths: commit.paths,
            })
            .collect(),
        identity,
    ))
}

/// One commit with the `message` template, or one commit per top-level
/// folder (and per file at the vault root) with a generated message.
fn deterministic_or_template(
    config: &GitConfig,
    candidates: &BatchCommitCandidates,
    messages: GitBatchMessages,
) -> Vec<BatchCommitGroup> {
    if candidates.paths.is_empty() {
        return Vec::new();
    }
    if messages == GitBatchMessages::Template {
        return vec![BatchCommitGroup {
            message: render_auto_commit_message(&config.message, BATCH_ACTION, &candidates.paths),
            paths: candidates.paths.clone(),
        }];
    }
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for path in &candidates.paths {
        let group = path
            .split_once('/')
            .map_or_else(|| path.clone(), |(top, _)| top.to_string());
        groups.entry(group).or_default().push(path.clone());
    }
    groups
        .into_iter()
        .map(|(group, paths)| BatchCommitGroup {
            message: format!("Update {group}"),
            paths,
        })
        .collect()
}

fn cancellation_check(cancellation: &SyncCancellationToken) -> Result<(), AppError> {
    if cancellation.is_cancelled() {
        Err(AppError::operation("the batched commit was cancelled"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::Ungated;
    use crate::sync_semantic::{SemanticAgentCommit, SemanticAgentOutput};
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git should launch");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).expect("utf-8 output")
    }

    fn vault(messages: &str) -> TempDir {
        let temp = TempDir::new().expect("temporary vault");
        let root = temp.path();
        git(root, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git(root, &["config", "user.name", "Vulcan Test"]);
        git(root, &["config", "user.email", "vulcan@example.com"]);
        fs::create_dir_all(root.join(".vulcan")).expect("config directory");
        fs::write(
            root.join(".vulcan/config.toml"),
            format!(
                "[git]\nauto_commit = true\nstrategy = \"batched\"\nbatch_messages = \"{messages}\"\nmessage = \"vault {{action}}: {{count}} files\"\n"
            ),
        )
        .expect("git configuration");
        fs::write(root.join(".gitignore"), ".vulcan/\n").expect("ignore");
        fs::write(root.join("Home.md"), "home\n").expect("home");
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "Initial"]);
        fs::create_dir_all(root.join("Projects")).expect("projects");
        fs::write(root.join("Home.md"), "home edited\n").expect("edit");
        fs::write(root.join("Projects/Plan.md"), "plan\n").expect("plan");
        fs::write(root.join("Projects/Notes.md"), "notes\n").expect("notes");
        temp
    }

    fn log(root: &Path) -> Vec<String> {
        git(root, &["log", "--format=%s"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn run(root: &Path, agent: Option<&dyn SemanticAgentProvider>) -> BatchedCommitReport {
        let cancellation = SyncCancellationToken::default();
        run_batched_commit(
            &VaultPaths::new(root),
            &BatchedCommitOptions {
                agent,
                agent_unavailable: None,
                capture_owner: Some(CaptureOwner::AutoCommit),
                permission_profile: Some("unrestricted"),
                gate: &Ungated,
                cancellation: &cancellation,
                verbosity: Verbosity::Quiet,
            },
        )
        .expect("batched commit")
    }

    /// Answers with a fixed plan and records what it was shown.
    struct FakeAgent {
        commits: Vec<SemanticAgentCommit>,
        seen: Mutex<Vec<SemanticAgentRequest>>,
        /// A file the agent writes while it "thinks", racing the batch.
        concurrent_edit: Option<std::path::PathBuf>,
    }

    impl FakeAgent {
        fn new(commits: &[(&str, &[&str])]) -> Self {
            Self {
                commits: commits
                    .iter()
                    .map(|(message, paths)| SemanticAgentCommit {
                        group: (*message).to_string(),
                        message: (*message).to_string(),
                        paths: paths.iter().map(|path| (*path).to_string()).collect(),
                    })
                    .collect(),
                seen: Mutex::new(Vec::new()),
                concurrent_edit: None,
            }
        }
    }

    impl SemanticAgentProvider for FakeAgent {
        fn identity(&self) -> SemanticAgentIdentity {
            SemanticAgentIdentity {
                provider: "fake".to_string(),
                model: "fixed-plan".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            request: &SemanticAgentRequest,
            _cancellation: &SyncCancellationToken,
        ) -> Result<SemanticAgentOutput, AppError> {
            self.seen.lock().unwrap().push(request.clone());
            if let Some(path) = &self.concurrent_edit {
                fs::write(path, "written during planning\n").expect("concurrent edit");
            }
            Ok(SemanticAgentOutput {
                commits: self.commits.clone(),
            })
        }
    }

    #[test]
    fn template_messages_make_one_commit() {
        let temp = vault("template");
        let report = run(temp.path(), None);

        assert_eq!(report.status, BatchedCommitStatus::Committed);
        assert_eq!(report.used_messages, Some(GitBatchMessages::Template));
        assert_eq!(log(temp.path()), ["vault batch: 3 files", "Initial"]);
        assert!(git(temp.path(), &["status", "--porcelain"]).is_empty());
        assert_eq!(run(temp.path(), None).status, BatchedCommitStatus::Clean);
    }

    #[test]
    fn deterministic_messages_commit_each_top_level_folder() {
        let temp = vault("deterministic");
        let report = run(temp.path(), None);

        assert_eq!(report.commits.len(), 2);
        assert_eq!(
            log(temp.path()),
            ["Update Projects", "Update Home.md", "Initial"]
        );
    }

    #[test]
    fn agent_groups_and_messages_are_committed_in_the_agent_order() {
        let temp = vault("agent");
        let agent = FakeAgent::new(&[
            (
                "Start the project plan",
                &["Projects/Plan.md", "Projects/Notes.md"],
            ),
            ("Revise the home note", &["Home.md"]),
        ]);
        let report = run(temp.path(), Some(&agent));

        assert_eq!(report.status, BatchedCommitStatus::Committed);
        assert_eq!(report.used_messages, Some(GitBatchMessages::Agent));
        assert_eq!(
            report.agent.as_ref().map(|agent| agent.model.as_str()),
            Some("fixed-plan")
        );
        assert!(report.fallback.is_none());
        assert_eq!(
            log(temp.path()),
            ["Revise the home note", "Start the project plan", "Initial"]
        );
        let seen = agent.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].target_revision, "working-tree");
        let paths = seen[0]
            .changes
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, ["Home.md", "Projects/Notes.md", "Projects/Plan.md"]);
    }

    #[test]
    fn an_agent_plan_that_omits_a_path_falls_back_to_deterministic_groups() {
        let temp = vault("agent");
        let agent = FakeAgent::new(&[("Only the home note", &["Home.md"])]);
        let report = run(temp.path(), Some(&agent));

        assert_eq!(report.status, BatchedCommitStatus::Committed);
        assert_eq!(report.used_messages, Some(GitBatchMessages::Deterministic));
        assert!(report
            .fallback
            .as_deref()
            .is_some_and(|reason| reason.contains("omitted")));
        assert_eq!(
            log(temp.path()),
            ["Update Projects", "Update Home.md", "Initial"]
        );
    }

    #[test]
    fn a_missing_agent_falls_back_and_says_why() {
        let temp = vault("agent");
        let report = run(temp.path(), None);

        assert_eq!(report.used_messages, Some(GitBatchMessages::Deterministic));
        assert_eq!(
            report.fallback.as_deref(),
            Some("no semantic agent is configured on this device")
        );
    }

    #[test]
    fn a_work_tree_that_changes_during_planning_is_regrouped_without_the_agent() {
        let temp = vault("agent");
        let mut agent = FakeAgent::new(&[(
            "Everything",
            &["Home.md", "Projects/Notes.md", "Projects/Plan.md"],
        )]);
        agent.concurrent_edit = Some(temp.path().join("Late.md"));
        let report = run(temp.path(), Some(&agent));

        assert_eq!(report.status, BatchedCommitStatus::Committed);
        assert_eq!(report.used_messages, Some(GitBatchMessages::Deterministic));
        assert_eq!(agent.seen.lock().unwrap().len(), 1);
        assert_eq!(
            log(temp.path()),
            [
                "Update Projects",
                "Update Late.md",
                "Update Home.md",
                "Initial"
            ]
        );
    }

    #[test]
    fn git_sync_ownership_and_per_write_strategy_make_no_commit() {
        let temp = vault("template");
        let cancellation = SyncCancellationToken::default();
        let owned = run_batched_commit(
            &VaultPaths::new(temp.path()),
            &BatchedCommitOptions {
                agent: None,
                agent_unavailable: None,
                capture_owner: Some(CaptureOwner::HiddenRefSync {
                    wiki_id: "notes".to_string(),
                }),
                permission_profile: Some("unrestricted"),
                gate: &Ungated,
                cancellation: &cancellation,
                verbosity: Verbosity::Quiet,
            },
        )
        .expect("owned report");
        assert_eq!(owned.status, BatchedCommitStatus::OwnedBySync);

        fs::write(
            temp.path().join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .expect("per-write configuration");
        assert_eq!(run(temp.path(), None).status, BatchedCommitStatus::Disabled);
        assert_eq!(log(temp.path()), ["Initial"]);
    }

    #[test]
    fn manual_staging_is_reported_and_left_alone() {
        let temp = vault("template");
        git(temp.path(), &["add", "Home.md"]);
        let report = run(temp.path(), None);

        assert_eq!(report.left_staged, ["Home.md".to_string()]);
        assert_eq!(
            report.commits[0].files,
            ["Projects/Notes.md", "Projects/Plan.md"]
        );
        assert_eq!(
            git(temp.path(), &["diff", "--cached", "--name-only"]),
            "Home.md\n"
        );
    }

    #[test]
    fn interval_is_clamped_to_supported_bounds() {
        let mut config = GitConfig {
            batch_interval_seconds: 1,
            ..GitConfig::default()
        };
        assert_eq!(
            effective_batch_interval_seconds(&config),
            MIN_BATCH_INTERVAL_SECONDS
        );
        config.batch_interval_seconds = u64::MAX;
        assert_eq!(
            effective_batch_interval_seconds(&config),
            MAX_BATCH_INTERVAL_SECONDS
        );
    }
}
