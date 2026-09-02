//! Reusable conformance harness for implementations of [`GitEngine`](crate::GitEngine).
//!
//! The fixture is created with an installed Git CLI on purpose: an engine only
//! conforms when the objects and refs it produces remain interoperable with
//! ordinary Git. The engine under test owns every operation after cloning.

use crate::{
    GitCaptureRequest, GitEngine, GitEngineError, GitEngineKind, GitOid, GitPushResult,
    GitRefCreateResult, GitRefDeleteResult, GitRefName, GitRefUpdateResult, GitRemote,
    GitTreeApplyAction,
};
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

const CONFORMANCE_VERSION: u32 = 1;

/// Evidence returned after one engine passes the versioned fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitEngineConformanceReport {
    pub version: u32,
    pub engine: GitEngineKind,
    pub cases: Vec<&'static str>,
    pub capture_tree: GitOid,
    pub conflict_paths: Vec<String>,
}

/// A failed conformance step with bounded, user-readable evidence.
#[derive(Debug)]
pub struct GitEngineConformanceError {
    step: &'static str,
    detail: String,
}

impl GitEngineConformanceError {
    fn new(step: &'static str, detail: impl Into<String>) -> Self {
        Self {
            step,
            detail: detail.into(),
        }
    }

    fn engine(step: &'static str, error: impl Display) -> Self {
        Self::new(step, error.to_string())
    }
}

impl Display for GitEngineConformanceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Git engine conformance failed during {}: {}",
            self.step, self.detail
        )
    }
}

impl Error for GitEngineConformanceError {}

/// Exercises the engine-independent repository contract against a fresh local
/// bare remote and checks the result with ordinary Git.
///
/// This intentionally covers only operations owned by [`GitEngine`]. Journal
/// interruption recovery and repository-lock contention belong to the shared
/// application transaction suite and therefore apply to every conforming
/// engine without being reimplemented here.
#[allow(clippy::too_many_lines)]
pub fn run_git_engine_conformance(
    engine: &dyn GitEngine,
) -> Result<GitEngineConformanceReport, GitEngineConformanceError> {
    let fixture = ConformanceFixture::new()?;
    let installation = engine
        .installation()
        .map_err(|error| GitEngineConformanceError::engine("installation", error))?;
    if installation.engine != engine.kind() {
        return Err(GitEngineConformanceError::new(
            "installation",
            "installation and engine kinds differ",
        ));
    }

    let worktree = fixture.root.path().join("engine-worktree");
    let repository = engine
        .clone_repository(&crate::GitCloneRequest {
            source: fixture.remote.display().to_string(),
            work_tree: worktree.clone(),
            git_dir: None,
            platform: crate::GitPlatformProfile::native(),
        })
        .map_err(|error| GitEngineConformanceError::engine("clone", error))?;
    reference_git(&worktree, &["config", "user.name", "Vulcan Conformance"])?;
    reference_git(
        &worktree,
        &["config", "user.email", "conformance@vulcan.invalid"],
    )?;
    let base = engine
        .head_commit(&repository)
        .map_err(|error| GitEngineConformanceError::engine("read cloned HEAD", error))?
        .ok_or_else(|| GitEngineConformanceError::new("read cloned HEAD", "HEAD is unborn"))?;
    let normal_index = reference_git_stdout(&worktree, &["write-tree"])?;

    fs::write(worktree.join("Home.md"), "captured\n")
        .map_err(|error| GitEngineConformanceError::new("prepare capture", error.to_string()))?;
    fs::write(worktree.join("Added.md"), "added\n")
        .map_err(|error| GitEngineConformanceError::new("prepare capture", error.to_string()))?;
    let first_tree = engine
        .snapshot_worktree_tree(&repository, Some(&base))
        .map_err(|error| GitEngineConformanceError::engine("snapshot worktree", error))?;
    let second_tree = engine
        .snapshot_worktree_tree(&repository, Some(&base))
        .map_err(|error| GitEngineConformanceError::engine("repeat worktree snapshot", error))?;
    require(
        first_tree == second_tree,
        "repeat worktree snapshot",
        "identical bytes produced different trees",
    )?;
    let candidate_ref = parse_ref("refs/vulcan/conformance/v1/candidate")?;
    let candidate = engine
        .capture_worktree(
            &repository,
            &GitCaptureRequest {
                base: Some(base.clone()),
                target_ref: candidate_ref,
                target_before: None,
                message: "Vulcan engine conformance capture\n".to_string(),
            },
        )
        .map_err(|error| GitEngineConformanceError::engine("capture worktree", error))?;
    require(
        candidate.tree == first_tree,
        "capture worktree",
        "capture tree differs from the stable snapshot tree",
    )?;
    require(
        reference_git_stdout(&worktree, &["write-tree"])? == normal_index,
        "capture worktree",
        "capture mutated the normal index",
    )?;

    let cas_ref = parse_ref("refs/vulcan/conformance/v1/cas")?;
    require(
        engine
            .create_ref(&repository, &cas_ref, &base)
            .map_err(|error| GitEngineConformanceError::engine("create ref", error))?
            == GitRefCreateResult::Created,
        "create ref",
        "new ref was not created",
    )?;
    require(
        engine
            .create_ref(&repository, &cas_ref, &candidate.commit)
            .map_err(|error| GitEngineConformanceError::engine("create-only ref", error))?
            == GitRefCreateResult::Exists,
        "create-only ref",
        "an existing ref was replaced",
    )?;
    require(
        engine
            .compare_and_swap_ref(
                &repository,
                &cas_ref,
                &candidate.commit,
                Some(&candidate.commit),
            )
            .map_err(|error| GitEngineConformanceError::engine("stale local CAS", error))?
            == GitRefUpdateResult::Stale,
        "stale local CAS",
        "a stale expected object updated the ref",
    )?;
    require(
        engine
            .compare_and_swap_ref(&repository, &cas_ref, &candidate.commit, Some(&base))
            .map_err(|error| GitEngineConformanceError::engine("local CAS", error))?
            == GitRefUpdateResult::Updated,
        "local CAS",
        "an exact expected object did not update the ref",
    )?;

    let remote = GitRemote::parse("origin")
        .map_err(|error| GitEngineConformanceError::engine("remote name", error))?;
    let live_ref = parse_ref("refs/heads/__vulcan-conformance/live")?;
    require(
        engine
            .push_ref(&repository, &remote, &base, &live_ref, None)
            .map_err(|error| GitEngineConformanceError::engine("initial live push", error))?
            == GitPushResult::Updated,
        "initial live push",
        "initial live ref was not created",
    )?;
    require(
        engine
            .push_ref(
                &repository,
                &remote,
                &candidate.commit,
                &live_ref,
                Some(&base),
            )
            .map_err(|error| GitEngineConformanceError::engine("leased live push", error))?
            == GitPushResult::Updated,
        "leased live push",
        "exact lease did not update the live ref",
    )?;
    require(
        engine
            .push_ref(&repository, &remote, &base, &live_ref, Some(&base))
            .map_err(|error| GitEngineConformanceError::engine("rejected live push", error))?
            == GitPushResult::Rejected,
        "rejected live push",
        "stale remote lease was accepted",
    )?;
    let fetched_ref = parse_ref("refs/vulcan/conformance/v1/fetched")?;
    require(
        engine
            .fetch_ref(&repository, &remote, &live_ref, &fetched_ref)
            .map_err(|error| GitEngineConformanceError::engine("fetch live ref", error))?
            == candidate.commit,
        "fetch live ref",
        "fetched live object differs from the pushed object",
    )?;

    let custom_ref = parse_ref("refs/vulcan-conformance/v1/hidden")?;
    require(
        engine
            .push_ref(&repository, &remote, &candidate.commit, &custom_ref, None)
            .map_err(|error| GitEngineConformanceError::engine("push custom ref", error))?
            == GitPushResult::Updated,
        "push custom ref",
        "custom ref was not created",
    )?;
    let custom_fetched_ref = parse_ref("refs/vulcan/conformance/v1/custom-fetched")?;
    require(
        engine
            .fetch_ref(&repository, &remote, &custom_ref, &custom_fetched_ref)
            .map_err(|error| GitEngineConformanceError::engine("fetch custom ref", error))?
            == candidate.commit,
        "fetch custom ref",
        "custom ref did not round-trip",
    )?;
    require(
        engine
            .delete_remote_ref(&repository, &remote, &custom_ref, &candidate.commit)
            .map_err(|error| GitEngineConformanceError::engine("delete custom ref", error))?
            == GitRefDeleteResult::Deleted,
        "delete custom ref",
        "exact remote deletion lease failed",
    )?;

    fs::write(worktree.join("Conflict.md"), "remote\n").map_err(|error| {
        GitEngineConformanceError::new("prepare remote candidate", error.to_string())
    })?;
    let remote_candidate = engine
        .capture_worktree(
            &repository,
            &GitCaptureRequest {
                base: Some(candidate.commit.clone()),
                target_ref: parse_ref("refs/vulcan/conformance/v1/remote")?,
                target_before: None,
                message: "Vulcan engine conformance remote\n".to_string(),
            },
        )
        .map_err(|error| GitEngineConformanceError::engine("capture remote candidate", error))?;
    engine
        .apply_tree(&repository, &remote_candidate.commit, &candidate.commit)
        .map_err(|error| GitEngineConformanceError::engine("restore merge base", error))?;
    fs::write(worktree.join("Conflict.md"), "local\n").map_err(|error| {
        GitEngineConformanceError::new("prepare local candidate", error.to_string())
    })?;
    let local_candidate = engine
        .capture_worktree(
            &repository,
            &GitCaptureRequest {
                base: Some(candidate.commit.clone()),
                target_ref: parse_ref("refs/vulcan/conformance/v1/local")?,
                target_before: None,
                message: "Vulcan engine conformance local\n".to_string(),
            },
        )
        .map_err(|error| GitEngineConformanceError::engine("capture local candidate", error))?;
    let merge = engine
        .merge_commits(
            &repository,
            &remote_candidate.commit,
            &local_candidate.commit,
        )
        .map_err(|error| GitEngineConformanceError::engine("merge divergent commits", error))?;
    require(
        !merge.clean && merge.base == Some(candidate.commit.clone()),
        "merge divergent commits",
        "overlapping edits did not produce the expected conflict and base",
    )?;
    require(
        merge.conflict_paths == ["Conflict.md"],
        "merge divergent commits",
        format!("unexpected conflict paths: {:?}", merge.conflict_paths),
    )?;

    let plan = engine
        .plan_tree_application(
            &repository,
            &local_candidate.commit,
            &remote_candidate.commit,
        )
        .map_err(|error| GitEngineConformanceError::engine("plan non-empty apply", error))?;
    require(
        plan.paths
            .iter()
            .any(|path| path.path == "Conflict.md" && path.action == GitTreeApplyAction::Update),
        "plan non-empty apply",
        "application plan omitted the tracked update",
    )?;
    engine
        .apply_tree(
            &repository,
            &local_candidate.commit,
            &remote_candidate.commit,
        )
        .map_err(|error| GitEngineConformanceError::engine("apply non-empty tree", error))?;
    require(
        engine
            .worktree_matches_tree(&repository, &remote_candidate.commit)
            .map_err(|error| GitEngineConformanceError::engine("verify applied tree", error))?,
        "verify applied tree",
        "applied worktree does not match the target tree",
    )?;
    require(
        reference_git_stdout(&worktree, &["write-tree"])? == normal_index,
        "apply non-empty tree",
        "application mutated the normal index",
    )?;
    fs::write(worktree.join("Conflict.md"), "drift\n").map_err(|error| {
        GitEngineConformanceError::new("prepare apply drift", error.to_string())
    })?;
    require(
        matches!(
            engine.apply_tree(
                &repository,
                &remote_candidate.commit,
                &local_candidate.commit
            ),
            Err(GitEngineError::WorktreeChanged)
        ),
        "reject apply drift",
        "application overwrote a worktree that no longer matched its precondition",
    )?;
    require(
        fs::read_to_string(worktree.join("Conflict.md")).map_err(|error| {
            GitEngineConformanceError::new("verify apply drift", error.to_string())
        })? == "drift\n",
        "verify apply drift",
        "failed application changed the drifted file",
    )?;

    require(
        engine
            .delete_ref(&repository, &cas_ref, &candidate.commit)
            .map_err(|error| GitEngineConformanceError::engine("delete local ref", error))?
            == GitRefDeleteResult::Deleted,
        "delete local ref",
        "exact local deletion lease failed",
    )?;
    reference_git(
        &worktree,
        &["cat-file", "-e", &format!("{}^{{tree}}", candidate.commit)],
    )?;
    require(
        reference_git_stdout(&fixture.remote, &["rev-parse", live_ref.as_str()])?
            == candidate.commit.as_str(),
        "ordinary Git interoperability",
        "ordinary Git did not read the engine-published live ref",
    )?;

    Ok(GitEngineConformanceReport {
        version: CONFORMANCE_VERSION,
        engine: engine.kind(),
        cases: vec![
            "clone",
            "stable_capture",
            "normal_index_isolation",
            "local_ref_compare_and_swap",
            "live_ref_transport",
            "custom_ref_transport",
            "divergent_merge_conflict",
            "safe_non_empty_apply",
            "ordinary_git_interoperability",
        ],
        capture_tree: candidate.tree,
        conflict_paths: merge.conflict_paths,
    })
}

struct ConformanceFixture {
    root: TempDir,
    remote: std::path::PathBuf,
}

impl ConformanceFixture {
    fn new() -> Result<Self, GitEngineConformanceError> {
        let root = TempDir::new()
            .map_err(|error| GitEngineConformanceError::new("create fixture", error.to_string()))?;
        let seed = root.path().join("seed");
        fs::create_dir(&seed).map_err(|error| {
            GitEngineConformanceError::new("create seed repository", error.to_string())
        })?;
        reference_git(&seed, &["-c", "init.defaultBranch=main", "init", "--quiet"])?;
        reference_git(&seed, &["config", "user.name", "Vulcan Conformance"])?;
        reference_git(
            &seed,
            &["config", "user.email", "conformance@vulcan.invalid"],
        )?;
        fs::write(seed.join("Home.md"), "base\n").map_err(|error| {
            GitEngineConformanceError::new("write seed repository", error.to_string())
        })?;
        fs::write(seed.join("Conflict.md"), "base\n").map_err(|error| {
            GitEngineConformanceError::new("write seed repository", error.to_string())
        })?;
        reference_git(&seed, &["add", "--all", "--", "."])?;
        reference_git(&seed, &["commit", "--quiet", "-m", "conformance base"])?;
        let remote = root.path().join("remote.git");
        reference_git(
            root.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                remote.to_str().ok_or_else(|| {
                    GitEngineConformanceError::new("create bare remote", "non-UTF-8 path")
                })?,
            ],
        )?;
        reference_git(
            &seed,
            &[
                "push",
                "--quiet",
                remote.to_str().ok_or_else(|| {
                    GitEngineConformanceError::new("seed bare remote", "non-UTF-8 path")
                })?,
                "HEAD:refs/heads/main",
            ],
        )?;
        reference_git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
        Ok(Self { root, remote })
    }
}

fn parse_ref(value: &str) -> Result<GitRefName, GitEngineConformanceError> {
    GitRefName::parse(value)
        .map_err(|error| GitEngineConformanceError::engine("parse fixture ref", error))
}

fn require(
    condition: bool,
    step: &'static str,
    detail: impl Into<String>,
) -> Result<(), GitEngineConformanceError> {
    if condition {
        Ok(())
    } else {
        Err(GitEngineConformanceError::new(step, detail))
    }
}

fn reference_git(current_dir: &Path, arguments: &[&str]) -> Result<(), GitEngineConformanceError> {
    let output = reference_git_output(current_dir, arguments)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(reference_git_failure(arguments, &output))
    }
}

fn reference_git_stdout(
    current_dir: &Path,
    arguments: &[&str],
) -> Result<String, GitEngineConformanceError> {
    let output = reference_git_output(current_dir, arguments)?;
    if !output.status.success() {
        return Err(reference_git_failure(arguments, &output));
    }
    String::from_utf8(output.stdout)
        .map(|stdout| stdout.trim().to_string())
        .map_err(|error| GitEngineConformanceError::new("run reference Git", error.to_string()))
}

fn reference_git_output(
    current_dir: &Path,
    arguments: &[&str],
) -> Result<Output, GitEngineConformanceError> {
    Command::new("git")
        .current_dir(current_dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_NAMESPACE")
        .args(arguments)
        .output()
        .map_err(|error| GitEngineConformanceError::new("run reference Git", error.to_string()))
}

fn reference_git_failure(arguments: &[&str], output: &Output) -> GitEngineConformanceError {
    GitEngineConformanceError::new(
        "run reference Git",
        format!(
            "`git {}` failed with {}: {}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GitCliEngine;

    #[test]
    fn installed_cli_engine_passes_the_reusable_conformance_suite() {
        let report = run_git_engine_conformance(&GitCliEngine::default())
            .expect("installed Git CLI engine should conform");

        assert_eq!(report.version, 1);
        assert_eq!(report.engine, GitEngineKind::Cli);
        assert_eq!(report.conflict_paths, ["Conflict.md"]);
        assert!(report.cases.contains(&"ordinary_git_interoperability"));
    }
}
