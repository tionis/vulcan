//! Evidence of the exact source bytes consumed by a registry load.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ControlSnapshot {
    pub controls: BTreeSet<PathBuf>,
    pub observed: BTreeMap<PathBuf, Option<String>>,
    pub conflicted: bool,
}

pub(super) fn revision(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl ControlSnapshot {
    pub fn observe(&mut self, path: &Path, bytes: Option<&[u8]>) {
        let value = bytes.map(revision);
        if let Some(previous) = self.observed.insert(path.to_path_buf(), value.clone()) {
            self.conflicted |= previous != value;
        }
    }

    pub fn matches(
        &self,
        observed: &BTreeMap<PathBuf, Option<String>>,
        controls: &BTreeSet<PathBuf>,
    ) -> bool {
        !self.conflicted
            && self.controls == *controls
            && self
                .observed
                .iter()
                .all(|(path, value)| observed.get(path) == Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_reads_cannot_hide_conflicting_observations() {
        let mut snapshot = ControlSnapshot::default();
        snapshot.observe(Path::new("schema.txt"), None);
        snapshot.observe(Path::new("schema.txt"), Some(b"{}"));
        snapshot.observe(Path::new("schema.txt"), None);
        assert!(!snapshot.matches(&snapshot.observed, &BTreeSet::new()));
        let mut snapshot = ControlSnapshot::default();
        snapshot.observe(Path::new("schema.txt"), Some(b"{}"));
        snapshot.observe(Path::new("schema.txt"), Some(b"{}"));
        assert!(snapshot.matches(&snapshot.observed, &BTreeSet::new()));
        assert!(!snapshot.matches(
            &snapshot.observed,
            &BTreeSet::from([PathBuf::from("new.md")])
        ));
    }
}
