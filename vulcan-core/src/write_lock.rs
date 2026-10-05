use crate::VaultPaths;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
#[cfg(any(test, target_os = "android"))]
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub struct WriteLockGuard {
    file: File,
    // Released after the vault lock, so queued readers enter only once the
    // write section is over.
    _intent: File,
}

#[derive(Debug)]
pub struct ReadLockGuard {
    file: File,
}

/// Advisory locks grant no priority, so a steady stream of overlapping
/// readers could starve a writer indefinitely. Writers therefore first take
/// the intent lock exclusively; readers pass through it (shared) on their way
/// to the vault lock and release it at once. A waiting writer thus blocks new
/// readers while those already inside drain. The vault lock is only ever held
/// exclusively by an intent holder, so the turnstile cannot deadlock.
fn open_lock(path: PathBuf) -> Result<File, std::io::Error> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

pub fn acquire_write_lock(paths: &VaultPaths) -> Result<WriteLockGuard, std::io::Error> {
    validate_lock_directory(paths)?;
    let path = lock_file_path(paths)?;
    let intent = open_lock(path.with_file_name(INTENT_FILE_NAME))?;
    intent.lock_exclusive()?;
    let file = open_lock(path)?;
    file.lock_exclusive()?;

    Ok(WriteLockGuard {
        file,
        _intent: intent,
    })
}

pub fn acquire_read_lock(paths: &VaultPaths) -> Result<ReadLockGuard, std::io::Error> {
    validate_lock_directory(paths)?;
    let path = lock_file_path(paths)?;
    let intent = open_lock(path.with_file_name(INTENT_FILE_NAME))?;
    fs2::FileExt::lock_shared(&intent)?;
    let file = open_lock(path)?;
    fs2::FileExt::lock_shared(&file)?;
    drop(intent);

    Ok(ReadLockGuard { file })
}

/// Take the shared vault lock only if no writer holds it, without passing
/// the writer turnstile; `None` while a write section is active.
pub fn try_acquire_read_lock(paths: &VaultPaths) -> Result<Option<ReadLockGuard>, std::io::Error> {
    validate_lock_directory(paths)?;
    let file = open_lock(lock_file_path(paths)?)?;
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(Some(ReadLockGuard { file })),
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Writer-preference turnstile beside the vault lock; device-local like it.
pub const INTENT_FILE_NAME: &str = "write.intent";

#[allow(clippy::unnecessary_wraps)] // Android path resolution and directory creation are fallible.
fn lock_file_path(paths: &VaultPaths) -> Result<PathBuf, std::io::Error> {
    #[cfg(target_os = "android")]
    {
        let state_root = crate::vulcan_user_state_dir().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "cannot determine the private Vulcan state directory for the Android vault lock",
            )
        })?;
        return private_lock_file_path(paths, &state_root);
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(paths.vulcan_dir().join("write.lock"))
    }
}

#[cfg(any(test, target_os = "android"))]
fn private_lock_file_path(
    paths: &VaultPaths,
    state_root: &Path,
) -> Result<PathBuf, std::io::Error> {
    let key = crate::paths::device_local_vault_key(paths.vault_root())?;
    let directory = state_root.join("locks").join(key);
    fs::create_dir_all(&directory)?;
    Ok(directory.join("write.lock"))
}

fn validate_lock_directory(paths: &VaultPaths) -> Result<(), std::io::Error> {
    let metadata = fs::symlink_metadata(paths.vulcan_dir()).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "missing {}. Run `vulcan init` in {} first",
                    paths.vulcan_dir().display(),
                    paths.vault_root().display()
                ),
            )
        } else {
            error
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "expected {} to be a directory. Run `vulcan init` after fixing it",
                paths.vulcan_dir().display()
            ),
        ));
    }
    Ok(())
}

impl Drop for WriteLockGuard {
    fn drop(&mut self) {
        // Every exclusive section advances the cooperating-write epoch before
        // releasing, so readers retaining state can detect any Vulcan write.
        let _ = advance_write_epoch(&self.file);
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// Monotonic count of completed exclusive write-lock sections for this vault,
/// across processes. Retained read state compares it to detect cooperating
/// writes without waiting for filesystem notifications. A missing lock file
/// reads as zero. Callers hold the shared read lock for a consistent value.
pub fn read_write_epoch(paths: &VaultPaths) -> Result<u64, std::io::Error> {
    let path = lock_file_path(paths)?;
    match File::open(path) {
        Ok(file) => read_epoch(&file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn read_epoch(mut file: &File) -> Result<u64, std::io::Error> {
    use std::io::{Read, Seek, SeekFrom};

    let mut bytes = [0; 8];
    file.seek(SeekFrom::Start(0))?;
    let mut filled = 0;
    while filled < bytes.len() {
        match file.read(&mut bytes[filled..])? {
            0 => return Ok(0),
            read => filled += read,
        }
    }
    Ok(u64::from_le_bytes(bytes))
}

fn advance_write_epoch(mut file: &File) -> Result<(), std::io::Error> {
    use std::io::{Seek, SeekFrom, Write};

    let next = read_epoch(file)?.wrapping_add(1);
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&next.to_le_bytes())
}

impl Drop for ReadLockGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_sections_advance_the_shared_epoch() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = VaultPaths::new(temporary.path());
        assert_eq!(read_write_epoch(&paths).unwrap(), 0);
        fs::create_dir(paths.vulcan_dir()).expect("coordination directory");
        drop(acquire_read_lock(&paths).expect("read lock"));
        assert_eq!(read_write_epoch(&paths).unwrap(), 0);
        drop(acquire_write_lock(&paths).expect("write lock"));
        drop(acquire_write_lock(&paths).expect("write lock"));
        assert_eq!(read_write_epoch(&paths).unwrap(), 2);
    }

    #[test]
    fn overlapping_readers_cannot_starve_a_writer() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir(paths.vulcan_dir()).expect("coordination directory");
        let stop = AtomicBool::new(false);
        let waited = std::thread::scope(|threads| {
            // Staggered readers keep the shared lock continuously held.
            for offset in 0..4 {
                let (paths, stop) = (&paths, &stop);
                threads.spawn(move || {
                    std::thread::sleep(Duration::from_millis(offset * 5));
                    while !stop.load(Ordering::SeqCst) {
                        let _guard = acquire_read_lock(paths).expect("read lock");
                        std::thread::sleep(Duration::from_millis(20));
                    }
                });
            }
            std::thread::sleep(Duration::from_millis(100));
            let start = Instant::now();
            let guard = acquire_write_lock(&paths).expect("write lock");
            let waited = start.elapsed();
            drop(guard);
            // Readers proceed again once the writer leaves.
            drop(acquire_read_lock(&paths).expect("read lock"));
            stop.store(true, Ordering::SeqCst);
            waited
        });
        assert!(waited < Duration::from_secs(1), "writer waited {waited:?}");
    }

    #[test]
    fn acquiring_a_lock_does_not_scaffold_or_change_vault_inputs() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir(paths.vulcan_dir()).expect("minimal coordination directory");

        let guard = acquire_write_lock(&paths).expect("write lock");

        assert!(paths.vulcan_dir().join("write.lock").is_file());
        assert!(paths.vulcan_dir().join(INTENT_FILE_NAME).is_file());
        assert!(!paths.gitignore_file().exists());
        assert!(!paths.reports_dir().exists());
        drop(guard);
    }

    #[test]
    fn private_lock_path_is_stable_for_equivalent_vault_paths() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault");
        let state = temporary.path().join("private-state");
        let direct = private_lock_file_path(&VaultPaths::new(&vault), &state).expect("lock path");
        fs::create_dir(vault.join("subdir")).expect("subdir");
        let equivalent = private_lock_file_path(
            &VaultPaths::new(vault.join(".").join("subdir").join("..")),
            &state,
        )
        .expect("equivalent lock path");
        assert_eq!(direct, equivalent);
        assert!(direct.starts_with(state));
        assert!(!vault.join(".vulcan/write.lock").exists());
    }
}
