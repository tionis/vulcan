//! `.gitignore` rules as a commit or tree records them.
//!
//! Sync stops tracking a path once a shared `.gitignore` starts ignoring it,
//! and every device keeps its own copy. The decision reads only the rules in
//! the trees being compared, never the worktree or device-local excludes
//! (`core.excludesFile`, `info/exclude`), so all devices reach the same
//! result. A path that was already ignored while tracked, such as one added
//! with `git add --force`, stays tracked.

use crate::{GitEngine, GitEngineError, GitOid, GitRepository};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The `.gitignore` matchers a revision holds for a set of directories.
#[derive(Default)]
struct TreeIgnores {
    by_directory: BTreeMap<String, Gitignore>,
}

impl TreeIgnores {
    /// Loads the `.gitignore` files on the ancestor directories of `paths`,
    /// reading only those blobs.
    fn load(
        engine: &dyn GitEngine,
        repository: &GitRepository,
        revision: &GitOid,
        paths: &[&str],
    ) -> Result<Self, GitEngineError> {
        let directories = paths
            .iter()
            .flat_map(|path| ancestor_directories(path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let files = directories
            .iter()
            .map(|directory| ignore_file(directory))
            .collect::<Vec<_>>();
        let present = directories
            .into_iter()
            .zip(engine.resolve_paths(repository, revision, &files)?)
            .filter_map(|(directory, object)| Some((directory, object?)))
            .collect::<Vec<_>>();
        if present.is_empty() {
            return Ok(Self::default());
        }
        let objects = present
            .iter()
            .map(|(_, object)| object.clone())
            .collect::<Vec<_>>();
        let blobs = engine.read_blobs(repository, &objects)?;
        Ok(Self::from_files(present.into_iter().filter_map(
            |(directory, object)| Some((directory, blobs.get(&object)?.clone())),
        )))
    }

    /// Loads the `.gitignore` files on the ancestor directories of `paths`
    /// from a work tree on disk.
    fn load_worktree(work_tree: &Path, paths: &[&str]) -> Self {
        let directories = paths
            .iter()
            .flat_map(|path| ancestor_directories(path))
            .collect::<BTreeSet<_>>();
        Self::from_files(directories.into_iter().filter_map(|directory| {
            let data = std::fs::read(work_tree.join(ignore_file(&directory))).ok()?;
            Some((directory, data))
        }))
    }

    fn from_files(files: impl Iterator<Item = (String, Vec<u8>)>) -> Self {
        let mut by_directory = BTreeMap::new();
        for (directory, data) in files {
            let mut builder = GitignoreBuilder::new(Path::new(&directory));
            for line in String::from_utf8_lossy(&data).lines() {
                // Git skips a pattern it cannot parse; so does sync.
                let _ = builder.add_line(None, line);
            }
            if let Ok(matcher) = builder.build() {
                by_directory.insert(directory, matcher);
            }
        }
        Self { by_directory }
    }

    /// Whether Git would ignore `path` as an untracked file. Like Git, a path
    /// inside an ignored directory is ignored, and a negation cannot re-include
    /// it.
    fn is_ignored(&self, path: &str) -> bool {
        let mut end = 0;
        loop {
            let next = path[end..].find('/').map(|offset| end + offset);
            let candidate = &path[..next.unwrap_or(path.len())];
            if self.matches(candidate, next.is_some()) {
                return true;
            }
            match next {
                Some(separator) => end = separator + 1,
                None => return false,
            }
        }
    }

    /// The decision of the deepest `.gitignore` with a matching pattern.
    fn matches(&self, path: &str, is_directory: bool) -> bool {
        for directory in ancestor_directories(path).iter().rev() {
            if let Some(matcher) = self.by_directory.get(directory) {
                let matched = matcher.matched(Path::new(path), is_directory);
                if matched.is_ignore() {
                    return true;
                }
                if matched.is_whitelist() {
                    return false;
                }
            }
        }
        false
    }
}

/// Files Obsidian keeps per device, relative to the vault root. Syncing them
/// only spreads one device's pane layout to the others.
pub const OBSIDIAN_DEVICE_FILES: [&str; 2] = [
    ".obsidian/workspace.json",
    ".obsidian/workspace-mobile.json",
];

/// Whether the `.gitignore` files that `revision` records ignore each of
/// the repository-relative `paths`, ignoring device-local excludes.
pub fn tree_ignores(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    revision: &GitOid,
    paths: &[String],
) -> Result<Vec<bool>, GitEngineError> {
    let paths = paths.iter().map(String::as_str).collect::<Vec<_>>();
    let ignores = TreeIgnores::load(engine, repository, revision, &paths)?;
    Ok(paths
        .into_iter()
        .map(|path| ignores.is_ignored(path))
        .collect())
}

/// Whether the `.gitignore` files in `work_tree` ignore each of the
/// repository-relative `paths`, ignoring device-local excludes.
#[must_use]
pub fn worktree_ignores(work_tree: &Path, paths: &[String]) -> Vec<bool> {
    let paths = paths.iter().map(String::as_str).collect::<Vec<_>>();
    let ignores = TreeIgnores::load_worktree(work_tree, &paths);
    paths
        .into_iter()
        .map(|path| ignores.is_ignored(path))
        .collect()
}

/// The paths among `paths` that the `.gitignore` files in `revision` ignore
/// while those in `previous` did not. Callers pass paths `previous` tracks.
pub(crate) fn newly_ignored(
    engine: &dyn GitEngine,
    repository: &GitRepository,
    previous: &GitOid,
    revision: &GitOid,
    paths: &[String],
) -> Result<Vec<String>, GitEngineError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let paths = paths.iter().map(String::as_str).collect::<Vec<_>>();
    let now = TreeIgnores::load(engine, repository, revision, &paths)?;
    let candidates = paths
        .into_iter()
        .filter(|path| now.is_ignored(path))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let before = TreeIgnores::load(engine, repository, previous, &candidates)?;
    Ok(candidates
        .into_iter()
        .filter(|path| !before.is_ignored(path))
        .map(str::to_string)
        .collect())
}

/// The directories whose `.gitignore` files can apply to `path`: the root
/// (`""`) and every parent, shallowest first.
fn ancestor_directories(path: &str) -> Vec<String> {
    let mut directories = vec![String::new()];
    let mut offset = 0;
    while let Some(separator) = path[offset..].find('/') {
        offset += separator;
        directories.push(path[..offset].to_string());
        offset += 1;
    }
    directories
}

/// The `.gitignore` path of a directory from [`ancestor_directories`].
pub(crate) fn ignore_file(directory: &str) -> String {
    if directory.is_empty() {
        ".gitignore".to_string()
    } else {
        format!("{directory}/.gitignore")
    }
}

/// The directory a `.gitignore` path governs, or `None` for other paths.
pub(crate) fn ignore_file_directory(path: &str) -> Option<&str> {
    if path == ".gitignore" {
        Some("")
    } else {
        path.strip_suffix("/.gitignore")
    }
}
