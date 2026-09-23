//! Canonical per-vault runtime ownership and alias reconciliation.

use crate::registry::{WikiId, WikiRegistration};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use ulid::Ulid;

const MAX_HOSTED_VAULTS: usize = 256;
const MAX_RUNTIME_ALIASES: usize = 1_024;
const MAX_INSTANCE_ID_BYTES: usize = 160;

type RuntimeMap = BTreeMap<PathBuf, VaultRuntimeDefinition>;
type AliasMap = BTreeMap<VaultRuntimeAlias, PathBuf>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporaryVaultRegistration {
    pub instance_id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum VaultRuntimeAlias {
    Registered(WikiId),
    Temporary(String),
}

impl Display for VaultRuntimeAlias {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registered(id) => write!(formatter, "wiki:{id}"),
            Self::Temporary(id) => write!(formatter, "instance:{id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultRuntimeOwner {
    Registered {
        registration_id: Ulid,
        wiki_id: WikiId,
    },
    Temporary {
        instance_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRuntimeDefinition {
    pub canonical_path: PathBuf,
    pub owner: VaultRuntimeOwner,
    pub aliases: BTreeSet<VaultRuntimeAlias>,
    /// The authoritative registered configuration, when this runtime has one.
    /// Plain Markdown and invocation-local runtimes intentionally have none.
    pub registration: Option<WikiRegistration>,
}

impl VaultRuntimeDefinition {
    #[must_use]
    pub fn is_git_backed(&self) -> bool {
        self.registration
            .as_ref()
            .and_then(|registration| registration.sync_backend.as_deref())
            == Some("git")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRuntimeReconcileReport {
    pub added: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub retained: Vec<PathBuf>,
    pub runtimes: Vec<VaultRuntimeDefinition>,
}

#[derive(Debug, Default)]
pub struct VaultRuntimeCatalog {
    runtimes: RuntimeMap,
    aliases: AliasMap,
}

impl VaultRuntimeCatalog {
    pub fn reconcile(
        &mut self,
        registrations: &[WikiRegistration],
        temporary: &[TemporaryVaultRegistration],
    ) -> Result<VaultRuntimeReconcileReport, VaultRuntimeError> {
        let (candidate, aliases) = build_candidate(registrations, temporary)?;
        let previous = self.runtimes.keys().cloned().collect::<BTreeSet<_>>();
        let next = candidate.keys().cloned().collect::<BTreeSet<_>>();
        let report = VaultRuntimeReconcileReport {
            added: next.difference(&previous).cloned().collect(),
            removed: previous.difference(&next).cloned().collect(),
            retained: next.intersection(&previous).cloned().collect(),
            runtimes: candidate.values().cloned().collect(),
        };
        self.runtimes = candidate;
        self.aliases = aliases;
        Ok(report)
    }

    #[must_use]
    pub fn runtimes(&self) -> Vec<VaultRuntimeDefinition> {
        self.runtimes.values().cloned().collect()
    }

    #[must_use]
    pub fn resolve(&self, alias: &VaultRuntimeAlias) -> Option<&VaultRuntimeDefinition> {
        self.aliases
            .get(alias)
            .and_then(|path| self.runtimes.get(path))
    }
}

#[derive(Debug)]
pub enum VaultRuntimeError {
    InvalidInstanceId(String),
    MissingVault { path: PathBuf, detail: String },
    TooManyRuntimes(usize),
    TooManyAliases(usize),
    DuplicateAlias(VaultRuntimeAlias),
}

impl Display for VaultRuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInstanceId(id) => {
                write!(formatter, "invalid temporary vault instance id `{id}`")
            }
            Self::MissingVault { path, detail } => {
                write!(formatter, "cannot host vault {}: {detail}", path.display())
            }
            Self::TooManyRuntimes(count) => write!(
                formatter,
                "host runtime count {count} exceeds the limit {MAX_HOSTED_VAULTS}"
            ),
            Self::TooManyAliases(count) => write!(
                formatter,
                "host vault alias count {count} exceeds the limit {MAX_RUNTIME_ALIASES}"
            ),
            Self::DuplicateAlias(alias) => write!(formatter, "duplicate vault alias `{alias}`"),
        }
    }
}

impl Error for VaultRuntimeError {}

fn build_candidate(
    registrations: &[WikiRegistration],
    temporary: &[TemporaryVaultRegistration],
) -> Result<(RuntimeMap, AliasMap), VaultRuntimeError> {
    let alias_count = registrations.len().saturating_add(temporary.len());
    if alias_count > MAX_RUNTIME_ALIASES {
        return Err(VaultRuntimeError::TooManyAliases(alias_count));
    }
    let mut grouped = BTreeMap::<PathBuf, RuntimeCandidate>::new();
    let mut aliases = BTreeMap::new();
    for registration in registrations {
        let path = canonical_vault(&registration.path)?;
        let alias = VaultRuntimeAlias::Registered(registration.id.clone());
        insert_alias(&mut aliases, alias.clone(), &path)?;
        let candidate = grouped.entry(path).or_default();
        candidate.aliases.insert(alias);
        candidate.registrations.push(registration.clone());
    }
    for registration in temporary {
        validate_instance_id(&registration.instance_id)?;
        let path = canonical_vault(&registration.path)?;
        let alias = VaultRuntimeAlias::Temporary(registration.instance_id.clone());
        insert_alias(&mut aliases, alias.clone(), &path)?;
        let candidate = grouped.entry(path).or_default();
        candidate.aliases.insert(alias);
        candidate
            .temporary_ids
            .push(registration.instance_id.clone());
    }
    if grouped.len() > MAX_HOSTED_VAULTS {
        return Err(VaultRuntimeError::TooManyRuntimes(grouped.len()));
    }
    let runtimes = grouped
        .into_iter()
        .map(|(canonical_path, candidate)| {
            let definition = candidate.finish(canonical_path.clone());
            (canonical_path, definition)
        })
        .collect();
    Ok((runtimes, aliases))
}

#[derive(Debug, Default)]
struct RuntimeCandidate {
    aliases: BTreeSet<VaultRuntimeAlias>,
    registrations: Vec<WikiRegistration>,
    temporary_ids: Vec<String>,
}

impl RuntimeCandidate {
    fn finish(mut self, canonical_path: PathBuf) -> VaultRuntimeDefinition {
        self.registrations
            .sort_by_key(|registration| registration.registration_id);
        self.temporary_ids.sort();
        let registration = self.registrations.into_iter().next();
        let owner = registration.as_ref().map_or_else(
            || VaultRuntimeOwner::Temporary {
                instance_id: self
                    .temporary_ids
                    .into_iter()
                    .next()
                    .expect("runtime candidate has an owner"),
            },
            |registration| VaultRuntimeOwner::Registered {
                registration_id: registration.registration_id,
                wiki_id: registration.id.clone(),
            },
        );
        VaultRuntimeDefinition {
            canonical_path,
            owner,
            aliases: self.aliases,
            registration,
        }
    }
}

fn insert_alias(
    aliases: &mut BTreeMap<VaultRuntimeAlias, PathBuf>,
    alias: VaultRuntimeAlias,
    path: &Path,
) -> Result<(), VaultRuntimeError> {
    if aliases.insert(alias.clone(), path.to_path_buf()).is_some() {
        return Err(VaultRuntimeError::DuplicateAlias(alias));
    }
    Ok(())
}

fn validate_instance_id(id: &str) -> Result<(), VaultRuntimeError> {
    if id.is_empty()
        || id.len() > MAX_INSTANCE_ID_BYTES
        || id.chars().any(char::is_control)
        || id.contains(['/', '\\'])
    {
        return Err(VaultRuntimeError::InvalidInstanceId(id.to_string()));
    }
    Ok(())
}

fn canonical_vault(path: &Path) -> Result<PathBuf, VaultRuntimeError> {
    let canonical = fs::canonicalize(path).map_err(|error| VaultRuntimeError::MissingVault {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;
    if !canonical.is_dir() {
        return Err(VaultRuntimeError::MissingVault {
            path: path.to_path_buf(),
            detail: "path is not a directory".to_string(),
        });
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn registration(id: &str, path: &Path, sync_backend: Option<&str>) -> WikiRegistration {
        WikiRegistration {
            profile: crate::registry::ManagedDirectoryProfile::Knowledge,
            profile_version: None,
            materialization: crate::registry::MaterializationProfile::Full,
            id: WikiId::parse(id).unwrap(),
            registration_id: Ulid::new(),
            path: path.to_path_buf(),
            groups: vec![],
            git_dir: None,
            permissions_profile: None,
            sync_backend: sync_backend.map(str::to_string),
            platform_profile: None,
            sync_paused: false,
        }
    }

    #[test]
    fn registered_and_temporary_aliases_share_one_canonical_owner() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let registered = registration("notes", &vault, Some("git"));
        let invocation = TemporaryVaultRegistration {
            instance_id: "preview-1".to_string(),
            path: vault.join("."),
        };
        let mut catalog = VaultRuntimeCatalog::default();
        let report = catalog
            .reconcile(std::slice::from_ref(&registered), &[invocation])
            .unwrap();

        assert_eq!(report.runtimes.len(), 1);
        let runtime = catalog
            .resolve(&VaultRuntimeAlias::Temporary("preview-1".to_string()))
            .unwrap();
        assert!(matches!(
            runtime.owner,
            VaultRuntimeOwner::Registered { ref wiki_id, .. } if wiki_id == &registered.id
        ));
        assert!(runtime.is_git_backed());
        assert_eq!(runtime.aliases.len(), 2);
    }

    #[test]
    fn plain_markdown_and_non_git_vaults_are_valid_runtimes() {
        let temporary = tempdir().unwrap();
        let registered_path = temporary.path().join("registered");
        let transient_path = temporary.path().join("transient");
        fs::create_dir(&registered_path).unwrap();
        fs::create_dir(&transient_path).unwrap();
        let plain = registration("plain", &registered_path, None);
        let transient = TemporaryVaultRegistration {
            instance_id: "serve".to_string(),
            path: transient_path,
        };
        let mut catalog = VaultRuntimeCatalog::default();
        let report = catalog.reconcile(&[plain], &[transient]).unwrap();

        assert_eq!(report.runtimes.len(), 2);
        assert!(report
            .runtimes
            .iter()
            .all(|runtime| !runtime.is_git_backed()));
    }

    #[test]
    fn reconcile_reports_add_remove_and_path_change_atomically() {
        let temporary = tempdir().unwrap();
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let original = registration("notes", &first, None);
        let mut moved = original.clone();
        moved.path = second.clone();
        let mut catalog = VaultRuntimeCatalog::default();
        let initial = catalog.reconcile(&[original], &[]).unwrap();
        assert_eq!(initial.added, vec![fs::canonicalize(&first).unwrap()]);

        let changed = catalog.reconcile(&[moved], &[]).unwrap();
        assert_eq!(changed.removed, vec![fs::canonicalize(&first).unwrap()]);
        assert_eq!(changed.added, vec![fs::canonicalize(&second).unwrap()]);
        assert!(changed.retained.is_empty());
    }

    #[test]
    fn invalid_candidate_does_not_replace_the_live_catalog() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let registered = registration("notes", &vault, None);
        let mut catalog = VaultRuntimeCatalog::default();
        catalog
            .reconcile(std::slice::from_ref(&registered), &[])
            .unwrap();

        let error = catalog
            .reconcile(
                &[],
                &[TemporaryVaultRegistration {
                    instance_id: "../invalid".to_string(),
                    path: vault,
                }],
            )
            .unwrap_err();
        assert!(matches!(error, VaultRuntimeError::InvalidInstanceId(_)));
        assert!(catalog
            .resolve(&VaultRuntimeAlias::Registered(registered.id))
            .is_some());
    }

    #[test]
    fn duplicate_aliases_are_rejected_before_catalog_mutation() {
        let temporary = tempdir().unwrap();
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let duplicates = [
            TemporaryVaultRegistration {
                instance_id: "preview".to_string(),
                path: first,
            },
            TemporaryVaultRegistration {
                instance_id: "preview".to_string(),
                path: second,
            },
        ];
        let mut catalog = VaultRuntimeCatalog::default();
        assert!(matches!(
            catalog.reconcile(&[], &duplicates),
            Err(VaultRuntimeError::DuplicateAlias(_))
        ));
        assert!(catalog.runtimes().is_empty());
    }
}
