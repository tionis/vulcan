//! Path scopes for path-scoped worktree synchronization.
//!
//! A scope is a set of literal repository-relative path prefixes. In
//! path-scoped mode the worktree is read and written only inside the scope;
//! everything outside it comes from committed history.

use serde::{Serialize, Serializer};
use std::fmt::{Display, Formatter};

/// Vulcan's own shared state that must travel with the scoped content.
const IMPLICIT_SCOPE_PATHS: [&str; 1] = [".vulcan/device-names"];

/// Device-local paths inside the scope that are never captured.
const SCOPE_CAPTURE_EXCLUDES: [&str; 8] = [
    ":(exclude,literal).vulcan/config.local.toml",
    ":(exclude,literal).vulcan/cache.db",
    ":(exclude,literal).vulcan/cache.db-wal",
    ":(exclude,literal).vulcan/cache.db-shm",
    ":(exclude,literal).vulcan/write.lock",
    ":(exclude,literal).vulcan/write.intent",
    ":(exclude,literal).vulcan/editor-watch.lock",
    ":(exclude,literal).vulcan/index-owner.lock",
];

/// A validated, normalized set of literal path prefixes. Each entry names a
/// file or a directory (and everything below it) relative to the repository
/// root, with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitWorktreeScope {
    entries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeScopeError(String);

impl Display for GitWorktreeScopeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for GitWorktreeScopeError {}

impl GitWorktreeScope {
    /// Validates and normalizes configured entries. Trailing slashes are
    /// dropped, duplicates and entries below another entry are removed, and
    /// Vulcan's shared device-name records are always included.
    pub fn new<I, S>(entries: I) -> Result<Self, GitWorktreeScopeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut normalized = Vec::new();
        for entry in entries {
            normalized.push(normalize_entry(entry.as_ref())?);
        }
        if normalized.is_empty() {
            return Err(GitWorktreeScopeError(
                "sync.paths must name at least one path for path-scoped synchronization"
                    .to_string(),
            ));
        }
        normalized.extend(IMPLICIT_SCOPE_PATHS.iter().map(ToString::to_string));
        normalized.sort();
        normalized.dedup();
        let mut entries: Vec<String> = Vec::new();
        for entry in normalized {
            if !entries.iter().any(|kept| covers(kept, &entry)) {
                entries.push(entry);
            }
        }
        Ok(Self { entries })
    }

    #[must_use]
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Whether a repository-relative path lies inside the scope.
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        self.entries.iter().any(|entry| covers(entry, path))
    }

    /// Literal pathspecs selecting the scope, followed by the device-local
    /// exclusions every capture applies.
    #[must_use]
    pub fn capture_pathspecs(&self) -> Vec<String> {
        let mut pathspecs = self.pathspecs();
        pathspecs.extend(SCOPE_CAPTURE_EXCLUDES.iter().map(ToString::to_string));
        pathspecs
    }

    /// Literal pathspecs selecting exactly the scope.
    #[must_use]
    pub fn pathspecs(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| format!(":(literal){entry}"))
            .collect()
    }

    /// Stable identity recorded in path-scoped live commits, so devices that
    /// disagree about the scope never merge each other's trees.
    #[must_use]
    pub fn hash(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for entry in &self.entries {
            hasher.update(entry.as_bytes());
            hasher.update(&[0]);
        }
        hasher.finalize().to_hex().to_string()
    }
}

impl Serialize for GitWorktreeScope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.entries.serialize(serializer)
    }
}

fn covers(entry: &str, path: &str) -> bool {
    path == entry
        || path
            .strip_prefix(entry)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn normalize_entry(raw: &str) -> Result<String, GitWorktreeScopeError> {
    let invalid = |reason: &str| {
        Err(GitWorktreeScopeError(format!(
            "sync.paths entry `{raw}` {reason}"
        )))
    };
    let entry = raw.trim_end_matches('/');
    if entry.is_empty() {
        return invalid("is empty; whole-tree synchronization is sync.worktree = \"all\"");
    }
    let drive = entry.as_bytes().get(1) == Some(&b':') && entry.as_bytes()[0].is_ascii_alphabetic();
    if entry.starts_with('/') || entry.contains('\\') || drive {
        return invalid("must be relative to the repository root and use `/` separators");
    }
    if entry.chars().any(char::is_control) {
        return invalid("contains a control character");
    }
    let components = entry.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| matches!(*component, "" | "." | ".."))
    {
        return invalid("must be a normalized path without empty, `.`, or `..` components");
    }
    if components.contains(&".git") {
        return invalid("names Git's own directory");
    }
    if components[0] == ".vulcan" {
        return invalid(
            "is Vulcan configuration, which changes only through commits in path-scoped mode",
        );
    }
    Ok(entry.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_normalized_deduplicated_and_include_device_names() {
        let scope = GitWorktreeScope::new(["wiki/", "Home.md", "wiki/Architecture", "wiki"])
            .expect("scope");

        assert_eq!(scope.entries(), [".vulcan/device-names", "Home.md", "wiki"]);
        assert!(scope.contains("wiki"));
        assert!(scope.contains("wiki/Architecture/Map.md"));
        assert!(scope.contains("Home.md"));
        assert!(scope.contains(".vulcan/device-names/vdev1_a.json"));
        assert!(!scope.contains("wikipedia.md"));
        assert!(!scope.contains("scripts/run.sh"));
        assert!(!scope.contains(".vulcan/config.toml"));
        assert_eq!(
            scope.pathspecs(),
            [
                ":(literal).vulcan/device-names",
                ":(literal)Home.md",
                ":(literal)wiki"
            ]
        );
    }

    #[test]
    fn hash_depends_only_on_the_normalized_entries() {
        let first = GitWorktreeScope::new(["wiki", "Home.md"]).expect("scope");
        let second = GitWorktreeScope::new(["Home.md", "wiki/", "wiki/a"]).expect("scope");
        let other = GitWorktreeScope::new(["wiki"]).expect("scope");

        assert_eq!(first.hash(), second.hash());
        assert_ne!(first.hash(), other.hash());
    }

    #[test]
    fn invalid_entries_are_rejected() {
        for entry in [
            "",
            "/",
            "/abs",
            "a//b",
            "./a",
            "a/../b",
            "..",
            ".git",
            "a/.git/b",
            ".vulcan",
            ".vulcan/config.toml",
            "a\\b",
            "C:/x",
            "a\nb",
        ] {
            assert!(
                GitWorktreeScope::new([entry]).is_err(),
                "`{entry}` should be rejected"
            );
        }
        assert!(GitWorktreeScope::new(Vec::<String>::new()).is_err());
        assert!(GitWorktreeScope::new(["notes/a b.md", "[literal]*"]).is_ok());
    }
}
