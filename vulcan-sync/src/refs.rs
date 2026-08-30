use crate::{GitEngineError, GitRefName, GitRemote};

pub const VULCAN_REF_NAMESPACE_VERSION: u32 = 1;
pub const DEFAULT_REMOTE_LIVE_REF: &str = "refs/heads/__vulcan-sync/live";
pub const REMOTE_EPOCH_BRANCH_ROOT: &str = "refs/heads/__vulcan-sync/epochs";
pub const LOCAL_VULCAN_REF_ROOT: &str = "refs/vulcan";

pub const LOCAL_RECOVERY_REF_NAMESPACES: &[&str] = &[
    "refs/vulcan/sync/",
    "refs/vulcan/epochs/",
    "refs/vulcan/conflicts/",
    "refs/vulcan/checkpoints/",
    "refs/vulcan/proposals/",
    "refs/vulcan/recovery/",
    // Retain the pre-contract roots in loss diagnostics so repositories
    // created by development builds are not falsely described as complete.
    "refs/vulcan/local/",
    "refs/vulcan/pending/",
    "refs/vulcan/semantic/",
];

#[must_use]
pub fn sync_profile_key(remote: &GitRemote, live_ref: &GitRefName) -> String {
    blake3::hash(format!("{remote}\0{live_ref}").as_bytes()).to_hex()[..16].to_string()
}

pub fn local_sync_ref(profile: &str, role: &str) -> Result<GitRefName, GitEngineError> {
    local_ref(&["sync", profile, role, "live"])
}

pub fn local_epoch_ref(profile: &str, epoch_id: &str) -> Result<GitRefName, GitEngineError> {
    local_ref(&["epochs", "live", profile, epoch_id])
}

pub fn remote_epoch_ref(profile: &str, epoch_id: &str) -> Result<GitRefName, GitEngineError> {
    GitRefName::parse(format!("{REMOTE_EPOCH_BRANCH_ROOT}/{profile}/{epoch_id}"))
}

pub fn conflict_ref(conflict_id: &str, role: &str) -> Result<GitRefName, GitEngineError> {
    local_ref(&["conflicts", conflict_id, role])
}

pub fn conflict_recovery_ref(
    conflict_id: &str,
    recovery_id: &str,
) -> Result<GitRefName, GitEngineError> {
    local_ref(&["conflicts", conflict_id, "recovery", recovery_id])
}

pub fn conflict_resolved_ref(conflict_id: &str) -> Result<GitRefName, GitEngineError> {
    local_ref(&["conflicts", conflict_id, "resolved"])
}

pub fn conflict_proposal_resolution_ref(
    conflict_id: &str,
    proposal_id: &str,
) -> Result<GitRefName, GitEngineError> {
    local_ref(&[
        "conflicts",
        conflict_id,
        "resolved",
        "proposals",
        proposal_id,
    ])
}

pub fn detached_recovery_ref(recovery_id: &str) -> Result<GitRefName, GitEngineError> {
    local_ref(&["recovery", "detached-git-loss", recovery_id])
}

pub fn checkpoint_ref(kind: &str, checkpoint_id: &str) -> Result<GitRefName, GitEngineError> {
    local_ref(&["checkpoints", kind, checkpoint_id])
}

pub fn semantic_proposal_ref(plan_id: &str) -> Result<GitRefName, GitEngineError> {
    local_ref(&["proposals", "semantic", plan_id])
}

#[must_use]
pub fn local_recovery_ref_namespaces() -> Vec<String> {
    LOCAL_RECOVERY_REF_NAMESPACES
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn local_ref(segments: &[&str]) -> Result<GitRefName, GitEngineError> {
    GitRefName::parse(format!("{LOCAL_VULCAN_REF_ROOT}/{}", segments.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_builders_are_versioned_stable_and_reject_unsafe_components() {
        assert_eq!(VULCAN_REF_NAMESPACE_VERSION, 1);
        let remote = GitRemote::parse("origin").expect("remote");
        let live = GitRefName::parse(DEFAULT_REMOTE_LIVE_REF).expect("live ref");
        let profile = sync_profile_key(&remote, &live);

        assert_eq!(profile.len(), 16);
        assert_eq!(
            local_sync_ref(&profile, "local")
                .expect("local ref")
                .as_str(),
            format!("refs/vulcan/sync/{profile}/local/live")
        );
        assert_eq!(
            remote_epoch_ref(&profile, "epoch")
                .expect("epoch ref")
                .as_str(),
            format!("refs/heads/__vulcan-sync/epochs/{profile}/epoch")
        );
        assert!(conflict_ref("../escape", "local").is_err());
        assert!(local_sync_ref(&profile, "bad role").is_err());
    }

    #[test]
    fn detached_loss_diagnostics_cover_every_current_local_root() {
        let namespaces = local_recovery_ref_namespaces();
        for expected in [
            "refs/vulcan/sync/",
            "refs/vulcan/epochs/",
            "refs/vulcan/conflicts/",
            "refs/vulcan/checkpoints/",
            "refs/vulcan/proposals/",
            "refs/vulcan/recovery/",
        ] {
            assert!(namespaces.iter().any(|namespace| namespace == expected));
        }
    }
}
