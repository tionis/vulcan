//! Registry-aware finite synchronization orchestration.

use crate::registry::{RegistryError, WikiId, WikiRegistration, WikiRegistry};
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use vulcan_app::sync::{sync_git_vault, GitSyncOptions, GitSyncOutcome, VaultSyncReport};
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisteredSyncSelection {
    Wiki(WikiId),
    Group(String),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredSyncItemReport {
    pub wiki_id: WikiId,
    pub path: std::path::PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<VaultSyncReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredSyncReport {
    pub selection: String,
    pub dry_run: bool,
    pub total: usize,
    pub succeeded: usize,
    pub conflicted: usize,
    pub failed: usize,
    pub items: Vec<RegisteredSyncItemReport>,
}

#[derive(Debug)]
pub enum RegisteredSyncError {
    Registry(RegistryError),
    EmptyGroup(String),
}

impl Display for RegisteredSyncError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::EmptyGroup(group) => {
                write!(formatter, "no registered wikis belong to group `{group}`")
            }
        }
    }
}

impl Error for RegisteredSyncError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) => Some(error),
            Self::EmptyGroup(_) => None,
        }
    }
}

impl From<RegistryError> for RegisteredSyncError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

pub fn sync_registered_wikis(
    registry: &WikiRegistry,
    selection: &RegisteredSyncSelection,
    options: &GitSyncOptions,
    permission_profile: Option<&str>,
) -> Result<RegisteredSyncReport, RegisteredSyncError> {
    let wikis = select_wikis(registry, selection)?;
    let mut items = Vec::with_capacity(wikis.len());
    for wiki in wikis {
        items.push(sync_registration(&wiki, options, permission_profile));
    }
    let conflicted = items
        .iter()
        .filter(|item| {
            item.report
                .as_ref()
                .is_some_and(|report| report.sync.outcome == GitSyncOutcome::Conflicted)
        })
        .count();
    let failed = items.iter().filter(|item| item.error.is_some()).count();
    let total = items.len();
    Ok(RegisteredSyncReport {
        selection: selection_label(selection),
        dry_run: options.dry_run,
        total,
        succeeded: total - failed - conflicted,
        conflicted,
        failed,
        items,
    })
}

fn select_wikis(
    registry: &WikiRegistry,
    selection: &RegisteredSyncSelection,
) -> Result<Vec<WikiRegistration>, RegisteredSyncError> {
    let config = registry.load()?;
    match selection {
        RegisteredSyncSelection::Wiki(id) => config
            .vaults
            .into_iter()
            .find(|wiki| &wiki.id == id)
            .map(|wiki| vec![wiki])
            .ok_or_else(|| RegistryError::UnknownWiki(id.clone()).into()),
        RegisteredSyncSelection::Group(group) => {
            let selected = config
                .vaults
                .into_iter()
                .filter(|wiki| wiki.groups.iter().any(|item| item == group))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(RegisteredSyncError::EmptyGroup(group.clone()));
            }
            Ok(selected)
        }
        RegisteredSyncSelection::All => Ok(config.vaults),
    }
}

fn sync_registration(
    wiki: &WikiRegistration,
    options: &GitSyncOptions,
    permission_profile: Option<&str>,
) -> RegisteredSyncItemReport {
    let paths = VaultPaths::new(&wiki.path);
    let result = resolve_permission_profile(&paths, permission_profile)
        .map_err(|error| error.to_string())
        .and_then(|selection| {
            ProfilePermissionGuard::new(&paths, selection)
                .check_git()
                .map_err(|error| error.to_string())
        })
        .and_then(|()| {
            if wiki
                .sync_backend
                .as_deref()
                .is_none_or(|backend| backend == "git")
            {
                sync_git_vault(&paths, options).map_err(|error| error.to_string())
            } else {
                Err(format!(
                    "wiki `{}` uses unsupported sync backend `{}`",
                    wiki.id,
                    wiki.sync_backend.as_deref().unwrap_or_default()
                ))
            }
        });
    match result {
        Ok(report) => RegisteredSyncItemReport {
            wiki_id: wiki.id.clone(),
            path: wiki.path.clone(),
            report: Some(report),
            error: None,
        },
        Err(error) => RegisteredSyncItemReport {
            wiki_id: wiki.id.clone(),
            path: wiki.path.clone(),
            report: None,
            error: Some(error),
        },
    }
}

fn selection_label(selection: &RegisteredSyncSelection) -> String {
    match selection {
        RegisteredSyncSelection::Wiki(id) => format!("wiki:{id}"),
        RegisteredSyncSelection::Group(group) => format!("group:{group}"),
        RegisteredSyncSelection::All => "all".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::AddWikiRequest;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn selection_is_sorted_and_empty_groups_are_explicit() {
        let temporary = tempdir().expect("temporary directory");
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        fs::create_dir(&first).expect("first wiki");
        fs::create_dir(&second).expect("second wiki");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        for (id, path, groups) in [
            ("work", second, vec!["team".to_string()]),
            ("personal", first, vec!["daily".to_string()]),
        ] {
            registry
                .add(
                    &AddWikiRequest {
                        id: WikiId::parse(id).expect("valid ID"),
                        path,
                        groups,
                        git_dir: None,
                        permissions_profile: None,
                        sync_backend: Some("git".to_string()),
                        platform_profile: None,
                    },
                    false,
                )
                .expect("register wiki");
        }

        let all = select_wikis(&registry, &RegisteredSyncSelection::All).expect("select all");
        assert_eq!(all[0].id.as_str(), "personal");
        assert_eq!(all[1].id.as_str(), "work");
        let daily = select_wikis(
            &registry,
            &RegisteredSyncSelection::Group("daily".to_string()),
        )
        .expect("select group");
        assert_eq!(daily.len(), 1);
        assert!(matches!(
            select_wikis(
                &registry,
                &RegisteredSyncSelection::Group("missing".to_string())
            ),
            Err(RegisteredSyncError::EmptyGroup(_))
        ));
    }
}
