//! Finds vaults in scan directories that are not registered yet.
//!
//! Discovery only suggests: it reads the registry and the directories, and
//! registering a candidate stays an explicit `vulcan vault add`. Only the
//! immediate subdirectories of a scan directory are considered, and one
//! counts as a vault when it holds `.obsidian/`, `.vulcan/`, or a
//! `.vulcan.toml` pointer. A plain folder of Markdown files is not proposed:
//! inside a scan directory that would sweep up every project folder.

use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::{resolve_registered_vault, RegistryError, WikiId, WikiRegistry};
use vulcan_core::vault_discovery::VAULT_POINTER_FILE_NAME;

/// What marks a directory as a vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultMarker {
    Obsidian,
    Vulcan,
    Pointer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredVault {
    /// The subdirectory that was found.
    pub path: PathBuf,
    /// The vault root it registers as; differs from `path` when a
    /// `.vulcan.toml` pointer names a directory below it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault_path: Option<PathBuf>,
    pub markers: Vec<VaultMarker>,
    /// The wiki already registered at this vault, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_as: Option<WikiId>,
    /// A free wiki ID derived from the directory name, for an unregistered
    /// vault; absent when the name yields no valid ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_id: Option<WikiId>,
    /// Why this candidate cannot be registered as it is (an unreadable or
    /// invalid pointer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScannedDirectory {
    pub path: PathBuf,
    /// Why the directory could not be read; it then contributes nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultDiscoveryReport {
    pub scan_dirs: Vec<ScannedDirectory>,
    pub vaults: Vec<DiscoveredVault>,
}

impl VaultDiscoveryReport {
    /// Candidates that could be registered now.
    pub fn unregistered(&self) -> impl Iterator<Item = &DiscoveredVault> {
        self.vaults
            .iter()
            .filter(|vault| vault.registered_as.is_none() && vault.error.is_none())
    }
}

/// Scans `dirs`, or the registry's remembered scan directories when `dirs`
/// is empty.
pub fn discover_vaults(
    registry: &WikiRegistry,
    dirs: &[PathBuf],
) -> Result<VaultDiscoveryReport, RegistryError> {
    let config = registry.load()?;
    let dirs = if dirs.is_empty() {
        config.scan_dirs.clone()
    } else {
        dirs.to_vec()
    };
    let mut taken: BTreeSet<String> = config
        .vaults
        .iter()
        .map(|wiki| wiki.id.as_str().to_string())
        .collect();
    let mut scan_dirs = Vec::new();
    let mut found = BTreeSet::new();
    for dir in dirs {
        match vault_subdirectories(&dir) {
            Ok(children) => {
                found.extend(children);
                scan_dirs.push(ScannedDirectory {
                    path: dir,
                    error: None,
                });
            }
            Err(error) => scan_dirs.push(ScannedDirectory {
                path: dir,
                error: Some(error.to_string()),
            }),
        }
    }
    let mut vaults = Vec::new();
    for (path, markers) in found {
        let (vault_path, error) = match resolve_registered_vault(&path) {
            Ok((vault_path, _)) => (Some(vault_path), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let registered_as = vault_path.as_ref().and_then(|vault_path| {
            config
                .vaults
                .iter()
                .find(|wiki| &wiki.path == vault_path)
                .map(|wiki| wiki.id.clone())
        });
        let suggested_id = (registered_as.is_none() && error.is_none())
            .then(|| suggest_id(&path, &taken))
            .flatten();
        if let Some(id) = &suggested_id {
            taken.insert(id.as_str().to_string());
        }
        vaults.push(DiscoveredVault {
            path,
            vault_path,
            markers,
            registered_as,
            suggested_id,
            error,
        });
    }
    Ok(VaultDiscoveryReport { scan_dirs, vaults })
}

/// The canonical immediate subdirectories of `dir` that carry a vault
/// marker. Hidden directories are skipped.
fn vault_subdirectories(dir: &Path) -> std::io::Result<Vec<(PathBuf, Vec<VaultMarker>)>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(path) = fs::canonicalize(entry.path()) else {
            continue;
        };
        if !path.is_dir() {
            continue;
        }
        let markers = vault_markers(&path);
        if !markers.is_empty() {
            found.push((path, markers));
        }
    }
    Ok(found)
}

fn vault_markers(path: &Path) -> Vec<VaultMarker> {
    [
        (VaultMarker::Obsidian, path.join(".obsidian").is_dir()),
        (VaultMarker::Vulcan, path.join(".vulcan").is_dir()),
        (
            VaultMarker::Pointer,
            path.join(VAULT_POINTER_FILE_NAME).is_file(),
        ),
    ]
    .into_iter()
    .filter_map(|(marker, present)| present.then_some(marker))
    .collect()
}

/// A wiki ID from the directory name that no registration or earlier
/// suggestion uses: lowercased, other characters folded to `-`, and a
/// numeric suffix on collision.
fn suggest_id(path: &Path, taken: &BTreeSet<String>) -> Option<WikiId> {
    let name = path.file_name()?.to_string_lossy();
    let mut base = String::new();
    for character in name.chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_' {
            base.push(character);
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let base = base.trim_matches(|c| c == '-' || c == '_');
    let base = &base[..base.len().min(56)];
    if base.is_empty() {
        return None;
    }
    // One more candidate than there are taken IDs always yields a free one.
    (1..=taken.len() + 1)
        .map(|n| {
            if n == 1 {
                base.to_string()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|candidate| !taken.contains(candidate))
        .and_then(|candidate| WikiId::parse(candidate).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::AddWikiRequest;

    fn registry(root: &Path) -> WikiRegistry {
        WikiRegistry::at(root.join("config/daemon.toml"))
    }

    fn add(registry: &WikiRegistry, id: &str, path: &Path) {
        registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse(id).unwrap(),
                    path: path.to_path_buf(),
                    profile: None,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("none".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .unwrap();
    }

    #[test]
    fn finds_marked_subdirectories_and_suggests_free_ids() {
        let temp = tempfile::tempdir().unwrap();
        let scan = temp.path().join("vaults");
        fs::create_dir_all(scan.join("My Notes/.obsidian")).unwrap();
        fs::create_dir_all(scan.join("work/.vulcan")).unwrap();
        fs::create_dir_all(scan.join("Work!/.obsidian")).unwrap();
        fs::create_dir_all(scan.join("plain")).unwrap();
        fs::write(scan.join("plain/note.md"), "# not a vault").unwrap();
        fs::create_dir_all(scan.join(".hidden/.obsidian")).unwrap();
        fs::create_dir_all(scan.join("registered/.obsidian")).unwrap();
        fs::create_dir_all(scan.join("repo/wiki")).unwrap();
        fs::write(scan.join("repo/.vulcan.toml"), "vault = \"wiki\"\n").unwrap();
        fs::create_dir_all(scan.join("broken")).unwrap();
        fs::write(scan.join("broken/.vulcan.toml"), "vault = \"missing\"\n").unwrap();
        let registry = registry(temp.path());
        add(&registry, "registered", &scan.join("registered"));
        add(&registry, "my-notes", &temp.path().join("vaults/plain"));

        let report = discover_vaults(&registry, std::slice::from_ref(&scan)).unwrap();
        let scan = fs::canonicalize(scan).unwrap();
        let by_name = |name: &str| {
            report
                .vaults
                .iter()
                .find(|vault| vault.path == scan.join(name))
                .unwrap_or_else(|| panic!("{name} was not discovered"))
        };
        assert_eq!(report.vaults.len(), 6, "{report:#?}");
        let notes = by_name("My Notes");
        assert_eq!(notes.markers, vec![VaultMarker::Obsidian]);
        assert_eq!(notes.suggested_id.as_ref().unwrap().as_str(), "my-notes-2");
        // Candidates are suggested in path order, so "Work!" claims `work`.
        assert_eq!(
            by_name("Work!").suggested_id.as_ref().unwrap().as_str(),
            "work"
        );
        assert_eq!(
            by_name("work").suggested_id.as_ref().unwrap().as_str(),
            "work-2"
        );
        let registered = by_name("registered");
        assert_eq!(
            registered.registered_as.as_ref().unwrap().as_str(),
            "registered"
        );
        assert!(registered.suggested_id.is_none());
        let repo = by_name("repo");
        assert_eq!(repo.markers, vec![VaultMarker::Pointer]);
        assert_eq!(repo.vault_path.as_ref().unwrap(), &scan.join("repo/wiki"));
        let broken = by_name("broken");
        assert!(broken.error.is_some() && broken.suggested_id.is_none());
        assert_eq!(report.unregistered().count(), 4);
    }

    #[test]
    fn uses_remembered_scan_dirs_and_reports_unreadable_ones() {
        let temp = tempfile::tempdir().unwrap();
        let scan = temp.path().join("vaults");
        fs::create_dir_all(scan.join("notes/.obsidian")).unwrap();
        let registry = registry(temp.path());
        let stored = registry.add_scan_dir(&scan, false).unwrap();
        assert_eq!(registry.add_scan_dir(&scan, false).unwrap(), stored);
        assert_eq!(registry.load().unwrap().scan_dirs, vec![stored.clone()]);

        let report = discover_vaults(&registry, &[]).unwrap();
        assert_eq!(report.scan_dirs.len(), 1);
        assert_eq!(report.vaults.len(), 1);

        let missing = temp.path().join("gone");
        let report = discover_vaults(&registry, std::slice::from_ref(&missing)).unwrap();
        assert_eq!(report.scan_dirs[0].path, missing);
        assert!(report.scan_dirs[0].error.is_some());
        assert!(report.vaults.is_empty());

        fs::remove_dir_all(&scan).unwrap();
        assert!(registry.add_scan_dir(&scan, false).is_err());
        assert_eq!(registry.remove_scan_dir(&stored, true).unwrap(), stored);
        assert_eq!(registry.load().unwrap().scan_dirs.len(), 1);
        registry.remove_scan_dir(&stored, false).unwrap();
        assert!(registry.load().unwrap().scan_dirs.is_empty());
        assert!(registry.remove_scan_dir(&stored, false).is_err());
    }

    #[test]
    fn suggested_ids_are_valid_or_absent() {
        let taken = BTreeSet::from(["notes".to_string()]);
        let id = |name: &str| suggest_id(Path::new(name), &taken).map(|id| id.as_str().to_string());
        assert_eq!(id("Notes").as_deref(), Some("notes-2"));
        assert_eq!(id("--Été 2024--").as_deref(), Some("t-2024"));
        assert_eq!(id("_x_").as_deref(), Some("x"));
        assert_eq!(id("日本"), None);
        assert_eq!(id(&"a".repeat(80)).unwrap().len(), 56);
    }
}
