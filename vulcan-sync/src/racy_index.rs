//! Detects a racily clean private sync index without spawning Git.
//!
//! Git cannot trust an index entry's cached stat data when the file's mtime is
//! not older than the index file itself, so every comparison re-reads that
//! file and runs its clean filter (for example Git LFS). Git's own index heals
//! when a later command rewrites it. Vulcan's private sync index is rewritten
//! only by captures, which usually run right after an edit, so a racy entry
//! could otherwise cost a full clean-filter pass on every unchanged sync.

use std::path::Path;
use std::time::UNIX_EPOCH;

/// Whether any entry of the index at `path` may be racily clean. Unreadable or
/// unrecognized indexes report `false`; this only gates an optimization.
pub(crate) fn may_have_racy_entries(path: &Path, hash_len: usize) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Some(index_seconds) = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs())
    else {
        return false;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    newest_entry_mtime(&bytes, hash_len).is_some_and(|newest| u64::from(newest) >= index_seconds)
}

/// The newest entry mtime (whole seconds) in a version 2, 3, or 4 index.
fn newest_entry_mtime(bytes: &[u8], hash_len: usize) -> Option<u32> {
    const STAT_BYTES: usize = 40;
    const EXTENDED_FLAG: u16 = 0x4000;
    if bytes.get(..4)? != b"DIRC" {
        return None;
    }
    let version = be32(bytes, 4)?;
    if !(2..=4).contains(&version) {
        return None;
    }
    let count = be32(bytes, 8)?;
    let mut offset = 12_usize;
    let mut newest = 0_u32;
    for _ in 0..count {
        newest = newest.max(be32(bytes, offset.checked_add(8)?)?);
        let flags_at = offset.checked_add(STAT_BYTES + hash_len)?;
        let flags = be16(bytes, flags_at)?;
        let mut name_at = flags_at + 2;
        if flags & EXTENDED_FLAG != 0 {
            if version < 3 {
                return None;
            }
            name_at += 2;
        }
        if version == 4 {
            // A varint prefix-strip length, then the NUL-terminated suffix.
            while bytes.get(name_at)? & 0x80 != 0 {
                name_at += 1;
            }
            name_at += 1;
            let suffix = bytes.get(name_at..)?.iter().position(|&byte| byte == 0)?;
            offset = name_at + suffix + 1;
        } else {
            // The name is NUL-padded so each entry spans a multiple of 8 bytes.
            let name = bytes.get(name_at..)?.iter().position(|&byte| byte == 0)?;
            offset += (name_at - offset + name + 8) & !7;
        }
    }
    Some(newest)
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::process::Command;
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;

    const SHA1_LEN: usize = 20;

    fn git(directory: &Path, index: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .env("GIT_INDEX_FILE", index)
            .status()
            .expect("run git");
        assert!(status.success(), "git {arguments:?} failed");
    }

    fn set_mtime(path: &Path, time: SystemTime) {
        File::options()
            .write(true)
            .open(path)
            .expect("open for mtime")
            .set_modified(time)
            .expect("set mtime");
    }

    /// An index over two files whose mtimes are `file_time`, written by Git.
    fn index_fixture(version: &str, file_time: SystemTime) -> (TempDir, std::path::PathBuf) {
        let temporary = TempDir::new().expect("temporary directory");
        // Keep the index outside the worktree so `add -A` cannot stage it.
        let index = temporary.path().join("private-index");
        let work = &temporary.path().join("work");
        fs::create_dir(work).expect("work directory");
        git(work, &index, &["init", "--quiet"]);
        let nested = work.join("a").join("deeply").join("nested");
        fs::create_dir_all(&nested).expect("nested directory");
        for path in [work.join("Home.md"), nested.join("long-name-note.md")] {
            fs::write(&path, "bytes\n").expect("write note");
            set_mtime(&path, file_time);
        }
        git(work, &index, &["add", "-A"]);
        git(work, &index, &["update-index", "--index-version", version]);
        (temporary, index)
    }

    #[test]
    fn entries_as_new_as_the_index_are_racy_in_every_index_version() {
        // A whole second, so adding less than a second stays within it.
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        let file_time = UNIX_EPOCH
            + Duration::from_secs(
                hour_ago
                    .duration_since(UNIX_EPOCH)
                    .expect("time after epoch")
                    .as_secs(),
            );
        for version in ["2", "3", "4"] {
            let (_temporary, index) = index_fixture(version, file_time);
            set_mtime(&index, file_time + Duration::from_millis(300));
            assert!(
                may_have_racy_entries(&index, SHA1_LEN),
                "index version {version} written in the same second must be racy"
            );
            set_mtime(&index, file_time + Duration::from_secs(5));
            assert!(
                !may_have_racy_entries(&index, SHA1_LEN),
                "index version {version} written later must not be racy"
            );
        }
    }

    #[test]
    fn unrecognized_indexes_are_never_reported_racy() {
        let temporary = TempDir::new().expect("temporary directory");
        let index = temporary.path().join("index");
        assert!(!may_have_racy_entries(&index, SHA1_LEN));
        fs::write(&index, b"not an index").expect("write junk");
        assert!(!may_have_racy_entries(&index, SHA1_LEN));
        let mut truncated = b"DIRC".to_vec();
        truncated.extend_from_slice(&2_u32.to_be_bytes());
        truncated.extend_from_slice(&5_u32.to_be_bytes());
        truncated.extend_from_slice(&[0; 30]);
        fs::write(&index, truncated).expect("write truncated");
        assert!(!may_have_racy_entries(&index, SHA1_LEN));
    }
}
