//! Agent-assisted replay of a diverged branch whose rebase stopped on a
//! conflict.
//!
//! A guarded rebase aborts on any conflict and pauses the branch. When the
//! device has consented to agent conflict resolution for the wiki, this
//! workflow replays the local commits onto the upstream in a scratch
//! worktree, asks the resolution agent for each conflicted commit, and moves
//! the branch only if every commit replayed cleanly. Nothing is pushed here;
//! the next synchronization publishes the replayed commits.

use crate::execution::MutationGate;
use crate::sync_proposals::{
    cancellation_check, ResolutionAgentFile, ResolutionAgentProvider, ResolutionAgentRequest,
    ResolutionAgentSide, VaultResolutionAgentTools, RESOLUTION_AGENT_TOOL_CONTRACT_VERSION,
};
use crate::AppError;
use serde::Serialize;
use vulcan_core::{load_vault_config, ProfilePermissionGuard, VaultPaths};
use vulcan_sync::{
    BranchPullPolicy, BranchReplayConflict, BranchReplayOutcome, BranchReplayResolver, GitEngine,
    GitOid, SyncCancellationToken,
};

/// What [`replay_diverged_branch`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum BranchReplayReport {
    /// The branch is not checked out, tracks nothing, or has not diverged.
    NotDiverged,
    /// The vault does not let synchronization rewrite local commits.
    NotPermitted { reason: String },
    /// The local commits now sit on top of the upstream.
    Replayed {
        from: GitOid,
        to: GitOid,
        upstream: GitOid,
        resolved: Vec<String>,
        /// `commit.gpgSign` is on, but the replayed commits are unsigned.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        signatures_dropped: bool,
    },
    /// Nothing changed; `head..upstream` identifies the attempt.
    Declined {
        head: GitOid,
        upstream: GitOid,
        reason: String,
    },
}

/// Replays the checked-out branch onto its upstream, resolving conflicts
/// with `provider`. Requires `sync.branch_pull` to be `rebase` or
/// `guarded-rebase`; the replay itself refuses commits another ref or
/// worktree builds on, merges, deletions, and non-text files. An attempt
/// whose `head..upstream` equals `declined` is not retried.
pub fn replay_diverged_branch(
    paths: &VaultPaths,
    provider: &dyn ResolutionAgentProvider,
    guard: ProfilePermissionGuard,
    declined: Option<&str>,
    cancellation: &SyncCancellationToken,
    gate: &dyn MutationGate,
) -> Result<BranchReplayReport, AppError> {
    let config = load_vault_config(paths).config;
    if !matches!(
        config.sync.branch_pull,
        BranchPullPolicy::Rebase | BranchPullPolicy::GuardedRebase
    ) {
        return Ok(BranchReplayReport::NotPermitted {
            reason:
                "set sync.branch_pull to guarded-rebase to let synchronization replay local commits"
                    .to_string(),
        });
    }
    let engine = vulcan_sync::GitCliEngine::default();
    let repository = engine
        .discover_repository(paths.vault_root())
        .map_err(AppError::operation)?;
    let Some(branch) = engine
        .head_reference(&repository)
        .map_err(AppError::operation)?
    else {
        return Ok(BranchReplayReport::NotDiverged);
    };
    let Some(upstream) = engine
        .branch_upstream(&repository, &branch)
        .map_err(AppError::operation)?
    else {
        return Ok(BranchReplayReport::NotDiverged);
    };
    let (Some(head), Some(upstream)) = (
        engine
            .read_ref(&repository, &branch)
            .map_err(AppError::operation)?,
        engine
            .read_ref(&repository, &upstream.tracking_ref)
            .map_err(AppError::operation)?,
    ) else {
        return Ok(BranchReplayReport::NotDiverged);
    };
    if engine
        .is_ancestor(&repository, &upstream, &head)
        .map_err(AppError::operation)?
        || engine
            .is_ancestor(&repository, &head, &upstream)
            .map_err(AppError::operation)?
    {
        return Ok(BranchReplayReport::NotDiverged);
    }
    if declined == Some(replay_attempt(&head, &upstream).as_str()) {
        return Ok(BranchReplayReport::Declined {
            head,
            upstream,
            reason: "this replay was already declined; it is retried once either side moves"
                .to_string(),
        });
    }
    cancellation_check(cancellation)?;
    let mut resolver = AgentReplayResolver {
        paths,
        provider,
        guard,
        cancellation,
        conflict_id: format!("branch-replay:{head}..{upstream}"),
    };
    // The replay and the agent's answers take as long as they take without
    // blocking the vault's writers or its synchronization; only the branch
    // move is serialized with them, and it rechecks that nothing moved.
    let prepared = match engine
        .prepare_branch_replay(&repository, &branch, &upstream, &mut resolver)
        .map_err(AppError::operation)?
    {
        Ok(prepared) => prepared,
        Err(reason) => {
            return Ok(BranchReplayReport::Declined {
                head,
                upstream,
                reason,
            })
        }
    };
    cancellation_check(cancellation)?;
    apply_prepared(&engine, &repository, &branch, upstream, prepared, gate)
}

/// Moves the branch to a prepared replay under the vault's mutation gate and
/// the repository lock, so it is serialized with writers and synchronization.
fn apply_prepared(
    engine: &vulcan_sync::GitCliEngine,
    repository: &vulcan_sync::GitRepository,
    branch: &vulcan_sync::GitRefName,
    upstream: GitOid,
    prepared: vulcan_sync::PreparedBranchReplay,
    gate: &dyn MutationGate,
) -> Result<BranchReplayReport, AppError> {
    let head = prepared.from.clone();
    let _permit = gate.enter()?;
    let _lock = vulcan_sync::RepositoryLock::acquire(&repository.git_dir).map_err(|error| {
        AppError::operation(format!(
            "could not lock the repository for the replay: {error:?}"
        ))
    })?;
    let outcome = engine
        .apply_branch_replay(repository, branch, prepared)
        .map_err(AppError::operation)?;
    Ok(match outcome {
        BranchReplayOutcome::Replayed {
            from,
            to,
            resolved,
            signatures_dropped,
        } => BranchReplayReport::Replayed {
            from,
            to,
            upstream,
            resolved,
            signatures_dropped,
        },
        BranchReplayOutcome::Declined { reason } => BranchReplayReport::Declined {
            head,
            upstream,
            reason,
        },
    })
}

/// Identifies one replay attempt in status reports.
#[must_use]
pub fn replay_attempt(head: &GitOid, upstream: &GitOid) -> String {
    format!("{head}..{upstream}")
}

struct AgentReplayResolver<'a> {
    paths: &'a VaultPaths,
    provider: &'a dyn ResolutionAgentProvider,
    guard: ProfilePermissionGuard,
    cancellation: &'a SyncCancellationToken,
    conflict_id: String,
}

impl BranchReplayResolver for AgentReplayResolver<'_> {
    fn resolve(
        &mut self,
        subject: &str,
        conflicts: &[BranchReplayConflict],
    ) -> Result<Vec<(String, String)>, String> {
        let side = |content: &Option<String>| ResolutionAgentSide {
            revision: None,
            mode: Some("100644".to_string()),
            content: content.as_ref().map(|text| text.clone().into_bytes()),
        };
        let request = ResolutionAgentRequest {
            conflict_id: format!("{} ({subject})", self.conflict_id),
            policy_version: 0,
            policy_hash: String::new(),
            selection: None,
            files: conflicts
                .iter()
                .map(|conflict| ResolutionAgentFile {
                    path: conflict.path.clone(),
                    base: side(&conflict.base),
                    local: side(&conflict.local),
                    remote: side(&conflict.upstream),
                })
                .collect(),
            focused_context: Vec::new(),
            broad_context_allowed: false,
            tool_contract_version: RESOLUTION_AGENT_TOOL_CONTRACT_VERSION,
        };
        let mut tools = VaultResolutionAgentTools::new(
            self.paths,
            self.guard.clone(),
            false,
            conflicts.iter().map(|conflict| conflict.path.clone()),
        );
        let output = self
            .provider
            .propose(&request, &mut tools, self.cancellation)
            .map_err(|error| error.to_string())?;
        output
            .paths
            .into_iter()
            .map(|path| {
                String::from_utf8(path.content)
                    .map(|content| (path.path.clone(), content))
                    .map_err(|_| {
                        format!("the agent returned non-UTF-8 content for `{}`", path.path)
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::Ungated;
    use crate::sync_proposals::{
        ResolutionAgentIdentity, ResolutionAgentOutput, ResolutionAgentPathOutput,
        ResolutionAgentTools,
    };
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Mutex;
    use vulcan_core::resolve_permission_profile;

    struct Merging(Mutex<Vec<ResolutionAgentRequest>>);

    impl ResolutionAgentProvider for Merging {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "test".to_string(),
                model: "merging".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            request: &ResolutionAgentRequest,
            _tools: &mut dyn ResolutionAgentTools,
            _cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, AppError> {
            self.0.lock().expect("requests").push(request.clone());
            Ok(ResolutionAgentOutput {
                explanation: "kept both".to_string(),
                referenced_context: Vec::new(),
                paths: request
                    .files
                    .iter()
                    .map(|file| ResolutionAgentPathOutput {
                        path: file.path.clone(),
                        content: b"local\nremote\n".to_vec(),
                    })
                    .collect(),
            })
        }
    }

    fn git(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(directory)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(arguments)
            .output()
            .expect("git");
        assert!(output.status.success(), "git {arguments:?}: {output:?}");
        String::from_utf8(output.stdout)
            .expect("utf-8")
            .trim()
            .to_string()
    }

    fn commit(directory: &Path, file: &str, content: &str, message: &str) {
        fs::write(directory.join(file), content).expect("write");
        git(directory, &["add", file]);
        git(directory, &["commit", "--quiet", "-m", message]);
    }

    /// A clone whose `Home.md` commit conflicts with a newer upstream one.
    fn diverged(policy: &str) -> (tempfile::TempDir, VaultPaths) {
        let temporary = tempfile::tempdir().expect("tempdir");
        let remote = temporary.path().join("remote.git");
        let other = temporary.path().join("other");
        let vault = temporary.path().join("vault");
        git(
            temporary.path(),
            &["init", "--quiet", "--bare", remote.to_str().unwrap()],
        );
        git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        commit(&other, "Home.md", "initial\n", "initial");
        git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
        git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                vault.to_str().unwrap(),
            ],
        );
        commit(&other, "Home.md", "remote\n", "remote edit");
        git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
        commit(&vault, "Home.md", "local\n", "local edit");
        git(&vault, &["fetch", "--quiet", "origin"]);
        fs::create_dir_all(vault.join(".vulcan")).expect("config dir");
        fs::write(
            vault.join(".vulcan/config.toml"),
            format!("[sync]\nbranch_pull = \"{policy}\"\n"),
        )
        .expect("config");
        fs::write(vault.join(".git/info/exclude"), ".vulcan/\n").expect("exclude");
        // The replay commits through Vulcan's own Git calls, which read the
        // repository's identity: CI runners have no global one.
        git(&vault, &["config", "user.name", "Test"]);
        git(&vault, &["config", "user.email", "test@example.com"]);
        (temporary, VaultPaths::new(&vault))
    }

    fn run(paths: &VaultPaths, provider: &Merging) -> BranchReplayReport {
        let selection = resolve_permission_profile(paths, Some("unrestricted")).expect("profile");
        replay_diverged_branch(
            paths,
            provider,
            ProfilePermissionGuard::new(paths, selection),
            None,
            &SyncCancellationToken::default(),
            &Ungated,
        )
        .expect("replay")
    }

    #[test]
    fn the_agent_settles_a_conflicted_guarded_rebase() {
        let (_temporary, paths) = diverged("guarded-rebase");
        let provider = Merging(Mutex::new(Vec::new()));

        let report = run(&paths, &provider);

        let BranchReplayReport::Replayed { to, resolved, .. } = report else {
            panic!("{report:?}");
        };
        let vault = paths.vault_root();
        assert_eq!(resolved, vec!["Home.md".to_string()]);
        assert_eq!(git(vault, &["rev-parse", "HEAD"]), to.as_str());
        assert_eq!(
            git(vault, &["rev-parse", "HEAD~1"]),
            git(vault, &["rev-parse", "origin/main"])
        );
        // Git for Windows checks text out with CRLF (`core.autocrlf`).
        assert_eq!(
            fs::read_to_string(vault.join("Home.md"))
                .unwrap()
                .replace("\r\n", "\n"),
            "local\nremote\n"
        );
        let requests = provider.0.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].conflict_id.contains("local edit"));
        assert_eq!(
            requests[0].files[0].local.content.as_deref(),
            Some(&b"local\n"[..])
        );
        assert_eq!(
            requests[0].files[0].remote.content.as_deref(),
            Some(&b"remote\n"[..])
        );

        assert_eq!(run(&paths, &provider), BranchReplayReport::NotDiverged);
    }

    #[test]
    fn only_a_rebasing_policy_lets_the_agent_rewrite_commits() {
        let (_temporary, paths) = diverged("ff-only");
        let provider = Merging(Mutex::new(Vec::new()));
        let head = git(paths.vault_root(), &["rev-parse", "HEAD"]);

        let report = run(&paths, &provider);

        assert!(
            matches!(report, BranchReplayReport::NotPermitted { .. }),
            "{report:?}"
        );
        assert!(provider.0.lock().unwrap().is_empty());
        assert_eq!(git(paths.vault_root(), &["rev-parse", "HEAD"]), head);
    }
}
