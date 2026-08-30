//! Registry-aware Git clone orchestration for managed wikis.

use crate::registry::{AddWikiRequest, RegistryError, WikiId, WikiRegistration, WikiRegistry};
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use vulcan_app::sync::{
    clone_git_vault, recover_detached_git_vault, GitCloneReport, GitCloneRequest,
    GitDetachedRecoveryReport, GitDetachedRecoveryRequest, GitPlatformPolicy, GitPlatformProfile,
};
use vulcan_sync::local_recovery_ref_namespaces;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneWikiRequest {
    pub id: WikiId,
    pub source: String,
    pub work_tree: PathBuf,
    pub git_dir: Option<PathBuf>,
    pub platform: GitPlatformProfile,
    pub groups: Vec<String>,
    pub permissions_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloneRegistrationPlan {
    pub id: WikiId,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions_profile: Option<String>,
    pub sync_backend: String,
    pub platform_profile: GitPlatformProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloneWikiReport {
    pub action: &'static str,
    pub dry_run: bool,
    pub source: String,
    pub platform_policy: GitPlatformPolicy,
    pub proposed_registration: CloneRegistrationPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clone: Option<GitCloneReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wiki: Option<WikiRegistration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverWikiGitRequest {
    pub id: WikiId,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoverWikiGitReport {
    pub action: &'static str,
    pub dry_run: bool,
    pub source: String,
    pub wiki: WikiRegistration,
    pub warning: String,
    pub possibly_lost_hidden_ref_namespaces: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<GitDetachedRecoveryReport>,
}

#[derive(Debug)]
pub enum CloneWikiError {
    Registry(RegistryError),
    InvalidDestination {
        path: PathBuf,
        detail: String,
    },
    Git(vulcan_app::AppError),
    RegistrationAfterClone {
        path: PathBuf,
        source: RegistryError,
    },
}

#[derive(Debug)]
pub enum RecoverWikiGitError {
    Registry(RegistryError),
    Unsupported(String),
    Git(vulcan_app::AppError),
}

impl Display for RecoverWikiGitError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Unsupported(detail) => formatter.write_str(detail),
            Self::Git(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for RecoverWikiGitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) => Some(error),
            Self::Git(error) => Some(error),
            Self::Unsupported(_) => None,
        }
    }
}

impl From<RegistryError> for RecoverWikiGitError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl Display for CloneWikiError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::InvalidDestination { path, detail } => {
                write!(formatter, "invalid clone destination {}: {detail}", path.display())
            }
            Self::Git(error) => Display::fmt(error, formatter),
            Self::RegistrationAfterClone { path, source } => write!(
                formatter,
                "Git clone completed at {}, but registration failed: {source}; cloned files were preserved",
                path.display()
            ),
        }
    }
}

impl Error for CloneWikiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) | Self::RegistrationAfterClone { source: error, .. } => {
                Some(error)
            }
            Self::Git(error) => Some(error),
            Self::InvalidDestination { .. } => None,
        }
    }
}

impl From<RegistryError> for CloneWikiError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

/// Plans or performs one clone followed by device-local registration.
///
/// A failed registration never causes Vulcan to remove a successfully cloned
/// worktree or Git directory. The error reports that recoverable partial state.
pub fn clone_registered_wiki(
    registry: &WikiRegistry,
    request: &CloneWikiRequest,
    dry_run: bool,
) -> Result<CloneWikiReport, CloneWikiError> {
    let work_tree = prospective_directory(&request.work_tree)?;
    let git_dir = request
        .git_dir
        .as_deref()
        .map(prospective_directory)
        .transpose()?;
    let mut groups = request.groups.clone();
    validate_groups(&groups)?;
    groups.sort();
    groups.dedup();
    preflight_registry(registry, &request.id, &work_tree, git_dir.as_deref())?;

    let proposed_registration = CloneRegistrationPlan {
        id: request.id.clone(),
        path: work_tree.clone(),
        groups: groups.clone(),
        git_dir: git_dir.clone(),
        permissions_profile: request.permissions_profile.clone(),
        sync_backend: "git".to_string(),
        platform_profile: request.platform,
    };
    let platform_policy = request.platform.policy();
    let source = redact_source(&request.source);
    let clone_request = GitCloneRequest {
        source: request.source.clone(),
        work_tree: work_tree.clone(),
        git_dir: git_dir.clone(),
        platform: request.platform,
    };
    clone_request
        .validate()
        .map_err(vulcan_app::AppError::operation)
        .map_err(CloneWikiError::Git)?;
    if dry_run {
        return Ok(CloneWikiReport {
            action: "clone",
            dry_run: true,
            source,
            platform_policy,
            proposed_registration,
            clone: None,
            wiki: None,
        });
    }

    let clone = clone_git_vault(&clone_request).map_err(CloneWikiError::Git)?;
    let wiki = registry
        .add(
            &AddWikiRequest {
                id: request.id.clone(),
                path: work_tree.clone(),
                groups,
                git_dir,
                permissions_profile: request.permissions_profile.clone(),
                sync_backend: Some("git".to_string()),
                platform_profile: Some(request.platform.as_str().to_string()),
            },
            false,
        )
        .map_err(|source| CloneWikiError::RegistrationAfterClone {
            path: work_tree,
            source,
        })?;
    Ok(CloneWikiReport {
        action: "clone",
        dry_run: false,
        source,
        platform_policy,
        proposed_registration,
        clone: Some(clone),
        wiki: Some(wiki),
    })
}

/// Recreates the missing detached Git directory for one registered wiki.
///
/// The materialized worktree is never checked out or reset. Its exact bytes
/// are captured in the replacement object database before the remote is
/// configured or fetched.
pub fn recover_registered_wiki_git(
    registry: &WikiRegistry,
    request: &RecoverWikiGitRequest,
    dry_run: bool,
) -> Result<RecoverWikiGitReport, RecoverWikiGitError> {
    let status = registry.show(&request.id)?;
    let wiki = status.registration;
    if wiki.sync_backend.as_deref().unwrap_or("git") != "git" {
        return Err(RecoverWikiGitError::Unsupported(format!(
            "wiki `{}` does not use the Git sync backend",
            wiki.id
        )));
    }
    let git_dir = wiki.git_dir.clone().ok_or_else(|| {
        RecoverWikiGitError::Unsupported(format!(
            "wiki `{}` has no registered detached Git directory",
            wiki.id
        ))
    })?;
    let platform = match wiki.platform_profile.as_deref() {
        Some("android_shared") => GitPlatformProfile::AndroidShared,
        Some("linux_native") => GitPlatformProfile::LinuxNative,
        Some("windows_native") => GitPlatformProfile::WindowsNative,
        Some("other_native") => GitPlatformProfile::OtherNative,
        None => GitPlatformProfile::native(),
        Some(value) => {
            return Err(RecoverWikiGitError::Unsupported(format!(
                "wiki `{}` has unknown platform profile `{value}`",
                wiki.id
            )))
        }
    };
    let recovery_request = GitDetachedRecoveryRequest {
        source: request.source.clone(),
        work_tree: wiki.path.clone(),
        git_dir,
        platform,
    };
    recovery_request
        .validate()
        .map_err(vulcan_app::AppError::operation)
        .map_err(RecoverWikiGitError::Git)?;
    let source = redact_source(&request.source);
    let warning = "The missing detached Git directory may have contained unpushed Vulcan hidden refs. The materialized worktree can be preserved, but refs and objects that existed only in the lost directory cannot be reconstructed. On Android, uninstalling Termux can remove its private Git data while leaving the shared Obsidian vault behind."
        .to_string();
    let possibly_lost_hidden_ref_namespaces = detached_loss_namespaces();
    if dry_run {
        return Ok(RecoverWikiGitReport {
            action: "recover_git",
            dry_run: true,
            source,
            wiki,
            warning,
            possibly_lost_hidden_ref_namespaces,
            recovery: None,
        });
    }
    let recovery =
        recover_detached_git_vault(&recovery_request).map_err(RecoverWikiGitError::Git)?;
    Ok(RecoverWikiGitReport {
        action: "recover_git",
        dry_run: false,
        source,
        wiki,
        warning,
        possibly_lost_hidden_ref_namespaces,
        recovery: Some(recovery),
    })
}

fn detached_loss_namespaces() -> Vec<String> {
    local_recovery_ref_namespaces()
}

fn prospective_directory(path: &Path) -> Result<PathBuf, CloneWikiError> {
    if path.exists() {
        return Err(CloneWikiError::InvalidDestination {
            path: path.to_path_buf(),
            detail: "destination already exists".to_string(),
        });
    }
    let file_name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| CloneWikiError::InvalidDestination {
            path: path.to_path_buf(),
            detail: "destination must name a new directory".to_string(),
        })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = match parent {
        Some(parent) => parent.to_path_buf(),
        None => std::env::current_dir().map_err(|error| CloneWikiError::InvalidDestination {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?,
    };
    let parent = fs::canonicalize(&parent).map_err(|error| CloneWikiError::InvalidDestination {
        path: path.to_path_buf(),
        detail: format!("parent directory is unavailable: {error}"),
    })?;
    Ok(parent.join(file_name))
}

fn validate_groups(groups: &[String]) -> Result<(), CloneWikiError> {
    for group in groups {
        WikiId::parse(group.clone()).map_err(|_| RegistryError::InvalidGroup(group.clone()))?;
    }
    Ok(())
}

fn preflight_registry(
    registry: &WikiRegistry,
    id: &WikiId,
    path: &Path,
    git_dir: Option<&Path>,
) -> Result<(), CloneWikiError> {
    let config = registry.load()?;
    if config.vaults.iter().any(|wiki| &wiki.id == id) {
        return Err(RegistryError::DuplicateId(id.clone()).into());
    }
    if let Some(existing) = config.vaults.iter().find(|wiki| wiki.path == path) {
        return Err(RegistryError::DuplicatePath {
            id: existing.id.clone(),
            path: path.to_path_buf(),
        }
        .into());
    }
    if let Some((existing, git_dir)) = git_dir.and_then(|git_dir| {
        config
            .vaults
            .iter()
            .find(|wiki| wiki.git_dir.as_deref() == Some(git_dir))
            .map(|wiki| (wiki, git_dir))
    }) {
        return Err(RegistryError::DuplicateGitDir {
            id: existing.id.clone(),
            path: git_dir.to_path_buf(),
        }
        .into());
    }
    Ok(())
}

fn redact_source(source: &str) -> String {
    let Some((scheme, remainder)) = source.split_once("://") else {
        return source.to_string();
    };
    let without_fragment = remainder.split(['?', '#']).next().unwrap_or(remainder);
    let (authority, suffix) = without_fragment
        .split_once('/')
        .map_or((without_fragment, ""), |(authority, suffix)| {
            (authority, suffix)
        });
    let authority = authority
        .rsplit_once('@')
        .map_or(authority.to_string(), |(_, host)| format!("***@{host}"));
    if suffix.is_empty() {
        format!("{scheme}://{authority}")
    } else {
        format!("{scheme}://{authority}/{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn request(root: &Path) -> CloneWikiRequest {
        CloneWikiRequest {
            id: WikiId::parse("personal").expect("valid ID"),
            source: "https://token@example.invalid/wiki.git?secret=yes".to_string(),
            work_tree: root.join("wiki"),
            git_dir: Some(root.join("git/wiki.git")),
            platform: GitPlatformProfile::AndroidShared,
            groups: vec!["mobile".to_string(), "mobile".to_string()],
            permissions_profile: None,
        }
    }

    #[test]
    fn dry_run_resolves_and_validates_without_mutation() {
        let temporary = tempdir().expect("temporary directory");
        fs::create_dir(temporary.path().join("git")).expect("Git parent");
        let registry_path = temporary.path().join("config/daemon.toml");
        let registry = WikiRegistry::at(registry_path.clone());

        let report = clone_registered_wiki(&registry, &request(temporary.path()), true)
            .expect("clone should plan");

        assert!(report.dry_run);
        assert_eq!(report.source, "https://***@example.invalid/wiki.git");
        assert_eq!(report.proposed_registration.groups, ["mobile"]);
        assert_eq!(
            report.platform_policy.profile,
            GitPlatformProfile::AndroidShared
        );
        assert!(!temporary.path().join("wiki").exists());
        assert!(!registry_path.exists());

        let mut invalid = request(temporary.path());
        invalid.work_tree = temporary.path().join("invalid");
        invalid.git_dir = None;
        invalid.source = "--upload-pack=unexpected".to_string();
        assert!(matches!(
            clone_registered_wiki(&registry, &invalid, true),
            Err(CloneWikiError::Git(_))
        ));
        assert!(!invalid.work_tree.exists());
    }

    #[test]
    fn dry_run_rejects_existing_destinations_and_duplicate_ids() {
        let temporary = tempdir().expect("temporary directory");
        fs::create_dir(temporary.path().join("git")).expect("Git parent");
        fs::create_dir(temporary.path().join("wiki")).expect("existing destination");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        assert!(matches!(
            clone_registered_wiki(&registry, &request(temporary.path()), true),
            Err(CloneWikiError::InvalidDestination { .. })
        ));

        let other = temporary.path().join("other");
        fs::create_dir(&other).expect("registered wiki");
        registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse("personal").expect("valid ID"),
                    path: other,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("registration");
        fs::remove_dir(temporary.path().join("wiki")).expect("remove existing destination");
        assert!(matches!(
            clone_registered_wiki(&registry, &request(temporary.path()), true),
            Err(CloneWikiError::Registry(RegistryError::DuplicateId(_)))
        ));
    }
}
