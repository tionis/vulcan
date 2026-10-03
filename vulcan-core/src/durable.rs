//! Crash-durable operational files.
//!
//! Vulcan keeps small records (registries, journals, credentials, trust
//! lists, daemon state) as individual files outside the vault. Every such
//! write goes through this module, so all of them share one contract:
//!
//! - a replacement is written to a temporary file in the target directory and
//!   renamed over the target, so readers see the old or the new bytes, never a
//!   mix;
//! - with [`Durability::Full`] the file and then its directory are synced, so
//!   the replacement survives power loss once the call returns;
//! - on Unix the file is owner-only (`0600`) before it becomes visible;
//! - symlinks and other non-regular files are refused, never followed;
//! - reads are bounded, so a corrupt or hostile file cannot exhaust memory.
//!
//! State that grows with the vault or with history belongs in a keyed store,
//! not in a file that is rewritten whole; see the design document.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use tempfile::NamedTempFile;

/// How strongly a write must survive a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// Sync the file and its directory before returning. Use for anything
    /// authoritative: losing it would lose user decisions or identity.
    #[default]
    Full,
    /// Atomic, but not synced. Use only for state that is cheap to recompute
    /// or merely informative, such as status reports.
    BestEffort,
}

/// Atomically replaces `path` with `bytes`, creating parent directories.
pub fn replace(path: &Path, bytes: &[u8], durability: Durability) -> io::Result<()> {
    let parent = prepare_parent(path)?;
    let temporary = staged(parent, bytes, durability)?;
    temporary.persist(path).map_err(|error| error.error)?;
    sync_directory(parent, durability)
}

/// Atomically creates `path` with `bytes` unless it already exists. Returns
/// whether the file was created.
pub fn create_new(path: &Path, bytes: &[u8], durability: Durability) -> io::Result<bool> {
    let parent = prepare_parent(path)?;
    let temporary = staged(parent, bytes, durability)?;
    match temporary.persist_noclobber(path) {
        Ok(_) => {
            sync_directory(parent, durability)?;
            Ok(true)
        }
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error),
    }
}

/// Removes `path` if it exists, syncing its directory. Returns whether a file
/// was removed.
pub fn remove(path: &Path, durability: Durability) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Err(not_regular(path)),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    }
    fs::remove_file(path)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent, durability)?;
    }
    Ok(true)
}

/// Reads at most `max_bytes` from the regular file at `path`. Returns `None`
/// when the file does not exist and an error when it is larger than the limit
/// or is not a regular file.
pub fn read_bounded(path: &Path, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Err(not_regular(path)),
        Ok(metadata) if metadata.len() > max_bytes => return Err(too_large(path, max_bytes)),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut bytes = Vec::new();
    // The limit is checked again while reading in case the file grew.
    File::open(path)?
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(too_large(path, max_bytes));
    }
    Ok(Some(bytes))
}

/// Atomically replaces `path` with `value` as pretty JSON and a final newline.
pub fn replace_json<T: Serialize + ?Sized>(
    path: &Path,
    value: &T,
    durability: Durability,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    replace(path, &bytes, durability)
}

/// Reads JSON from `path` within `max_bytes`. Returns `None` when the file
/// does not exist and [`io::ErrorKind::InvalidData`] when it does not parse.
pub fn read_json<T: DeserializeOwned>(path: &Path, max_bytes: u64) -> io::Result<Option<T>> {
    read_bounded(path, max_bytes)?
        .map(|bytes| {
            serde_json::from_slice(&bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not valid JSON: {error}", path.display()),
                )
            })
        })
        .transpose()
}

fn prepare_parent(path: &Path) -> io::Result<&Path> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => Err(not_regular(path)),
        Ok(_) => Ok(parent),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(parent),
        Err(error) => Err(error),
    }
}

fn staged(parent: &Path, bytes: &[u8], durability: Durability) -> io::Result<NamedTempFile> {
    let mut temporary = NamedTempFile::new_in(parent)?;
    restrict_to_owner(temporary.path())?;
    temporary.write_all(bytes)?;
    if durability == Durability::Full {
        temporary.as_file().sync_all()?;
    }
    Ok(temporary)
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

/// Windows temporary files already inherit the per-user state directory's
/// ACL; callers that need an explicit owner-only ACL apply it themselves.
#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // One fallible contract across platforms.
fn restrict_to_owner(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(directory: &Path, durability: Durability) -> io::Result<()> {
    if durability == Durability::Full {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

/// Windows cannot open a directory to sync it; the rename itself is durable
/// once `MoveFileEx` returns.
#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // One fallible contract across platforms.
fn sync_directory(_directory: &Path, _durability: Durability) -> io::Result<()> {
    Ok(())
}

fn not_regular(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} is not a regular file", path.display()),
    )
}

fn too_large(path: &Path, max_bytes: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} exceeds the {max_bytes}-byte limit", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn replace_creates_parents_and_replaces_atomically() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("nested/state.json");
        replace_json(&path, &vec!["first"], Durability::Full).expect("create");
        replace_json(&path, &vec!["second"], Durability::BestEffort).expect("replace");
        assert_eq!(
            read_json::<Vec<String>>(&path, 1024).expect("read"),
            Some(vec!["second".to_string()])
        );
        assert_eq!(
            fs::read_to_string(&path).expect("raw").chars().last(),
            Some('\n')
        );
        let leftovers = fs::read_dir(path.parent().expect("parent"))
            .expect("list")
            .count();
        assert_eq!(leftovers, 1, "no temporary file may remain");
    }

    #[cfg(unix)]
    #[test]
    fn written_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("secret.json");
        replace(&path, b"{}", Durability::Full).expect("write");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn create_new_never_clobbers() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("record");
        assert!(create_new(&path, b"one", Durability::Full).expect("create"));
        assert!(!create_new(&path, b"two", Durability::Full).expect("exists"));
        assert_eq!(fs::read(&path).expect("read"), b"one");
        assert!(remove(&path, Durability::Full).expect("remove"));
        assert!(!remove(&path, Durability::Full).expect("missing"));
    }

    #[test]
    fn reads_are_bounded_and_missing_files_are_none() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("state");
        assert_eq!(read_bounded(&path, 4).expect("missing"), None);
        replace(&path, b"12345", Durability::Full).expect("write");
        assert_eq!(
            read_bounded(&path, 4).expect_err("too large").kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            read_bounded(&path, 5).expect("fits"),
            Some(b"12345".to_vec())
        );
        replace(&path, b"not json", Durability::Full).expect("write");
        assert_eq!(
            read_json::<serde_json::Value>(&path, 64)
                .expect_err("malformed")
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_refused_not_followed() {
        let temporary = tempdir().expect("temporary directory");
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"keep").expect("outside file");
        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        assert!(replace(&link, b"overwrite", Durability::Full).is_err());
        assert!(read_bounded(&link, 64).is_err());
        assert!(remove(&link, Durability::Full).is_err());
        assert_eq!(fs::read(&outside).expect("outside"), b"keep");
    }
}
