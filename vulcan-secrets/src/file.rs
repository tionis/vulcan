use crate::{
    CapabilitySupport, SecretBytes, SecretName, SecretStore, SecretStoreCapabilities,
    SecretStoreError, SecretStoreState, MAX_SECRET_BYTES,
};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use zeroize::Zeroizing;

/// Portable owner-only file custody, not encrypted or hardware-backed storage.
/// The trusted caller chooses a device-local root and trusted parent directory.
/// Construction and inspection never create or repair that directory.
#[derive(Clone)]
pub struct ProtectedFileSecretStore {
    directory: PathBuf,
}

impl std::fmt::Debug for ProtectedFileSecretStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProtectedFileSecretStore([PRIVATE LOCATOR])")
    }
}

impl ProtectedFileSecretStore {
    #[must_use]
    pub fn at(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    fn path(&self, name: &SecretName) -> PathBuf {
        self.directory.join(format!("{}.secret", name.as_str()))
    }

    fn directory(&self) -> Result<File, SecretStoreError> {
        let metadata = fs::symlink_metadata(&self.directory).map_err(io_error)?;
        reject_link(&metadata)?;
        if !metadata.is_dir() {
            return Err(SecretStoreError::Invalid);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        no_follow(&mut options);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0200_0000 | 0x0020_0000); // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        }
        let file = options.open(&self.directory).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        reject_link(&metadata)?;
        if !metadata.is_dir() {
            return Err(SecretStoreError::Invalid);
        }
        owner_only(&file)?;
        Ok(file)
    }

    fn prepare_directory(&self) -> Result<File, SecretStoreError> {
        match self.directory() {
            Ok(file) => Ok(file),
            Err(SecretStoreError::Missing) => {
                create_private_directory(&self.directory)?;
                self.directory()
            }
            Err(error) => Err(error),
        }
    }

    fn mutation_lock(&self) -> Result<File, SecretStoreError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        no_follow(&mut options);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(self.directory.join(".store.lock"))
            .map_err(io_error)?;
        regular_file(&file)?;
        owner_only(&file)?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                SecretStoreError::Locked
            } else {
                io_error(error)
            }
        })?;
        Ok(file)
    }

    fn open_value(&self, name: &SecretName) -> Result<File, SecretStoreError> {
        open_protected_input(&self.path(name))
    }
}

impl SecretStore for ProtectedFileSecretStore {
    fn capabilities(&self) -> SecretStoreCapabilities {
        let support = if cfg!(any(unix, windows)) {
            CapabilitySupport::Supported
        } else {
            CapabilitySupport::Unsupported
        };
        SecretStoreCapabilities {
            persistent: support,
            device_local: support,
            exportable: support,
            unattended: support,
        }
    }

    fn inspect(&self, name: &SecretName) -> SecretStoreState {
        self.directory()
            .and_then(|_| self.open_value(name))
            .map_or_else(SecretStoreError::state, |_| SecretStoreState::Available)
    }

    fn create(&self, name: &SecretName, value: &SecretBytes) -> Result<(), SecretStoreError> {
        let directory = self.prepare_directory()?;
        let _lock = self.mutation_lock()?;
        let path = self.path(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Err(SecretStoreError::AlreadyExists),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        let mut staged = NamedTempFile::new_in(&self.directory).map_err(io_error)?;
        owner_only(staged.as_file())?;
        staged.write_all(value.expose()).map_err(io_error)?;
        staged.as_file().sync_all().map_err(io_error)?;
        staged.persist_noclobber(path).map_err(|error| {
            if error.error.kind() == io::ErrorKind::AlreadyExists {
                SecretStoreError::AlreadyExists
            } else {
                io_error(error.error)
            }
        })?;
        // Failure after publication is an unknown outcome, never permission to
        // generate a replacement credential or switch to another provider.
        sync_directory(&directory).map_err(|_| SecretStoreError::Unknown)
    }

    fn get(&self, name: &SecretName) -> Result<SecretBytes, SecretStoreError> {
        let _directory = self.directory()?;
        read_protected_secret_input(&self.path(name))
    }

    fn delete(&self, name: &SecretName) -> Result<(), SecretStoreError> {
        let directory = self.directory()?;
        let _lock = self.mutation_lock()?;
        let _file = self.open_value(name)?;
        fs::remove_file(self.path(name)).map_err(io_error)?;
        sync_directory(&directory).map_err(|_| SecretStoreError::Unknown)
    }
}

/// Metadata-only inspection for explicit trusted application import workflows.
/// It never creates files, repairs permissions, or reads the secret's bytes.
#[must_use]
pub fn inspect_protected_secret_input(path: &Path) -> SecretStoreState {
    open_protected_input(path).map_or_else(SecretStoreError::state, |_| SecretStoreState::Available)
}

/// Read one bounded owner-only source for an explicitly authorized import.
/// The application selects this trusted path; it is not a provider fallback or
/// a filesystem-read API for remote clients, plugins, or JavaScript.
pub fn read_protected_secret_input(path: &Path) -> Result<SecretBytes, SecretStoreError> {
    let file = open_protected_input(path)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_SECRET_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    SecretBytes::from_zeroizing(bytes)
}

fn open_protected_input(path: &Path) -> Result<File, SecretStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(io_error)?;
    reject_link(&metadata)?;
    if !metadata.is_file() {
        return Err(SecretStoreError::Invalid);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let file = options.open(path).map_err(io_error)?;
    regular_file(&file)?;
    owner_only(&file)?;
    let length = file.metadata().map_err(io_error)?.len();
    if length == 0 || length > MAX_SECRET_BYTES as u64 {
        return Err(SecretStoreError::Invalid);
    }
    Ok(file)
}

// Result::map_err passes ownership here; never retain or render its raw locator.
#[allow(clippy::needless_pass_by_value)]
fn io_error(error: io::Error) -> SecretStoreError {
    match error.kind() {
        io::ErrorKind::NotFound => SecretStoreError::Missing,
        io::ErrorKind::PermissionDenied => SecretStoreError::Denied,
        io::ErrorKind::TimedOut | io::ErrorKind::NotConnected => SecretStoreError::Unavailable,
        _ => SecretStoreError::Unknown,
    }
}

fn reject_link(metadata: &fs::Metadata) -> Result<(), SecretStoreError> {
    if metadata.file_type().is_symlink() {
        return Err(SecretStoreError::Invalid);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x0400 != 0 {
            return Err(SecretStoreError::Invalid);
        }
    }
    Ok(())
}

fn regular_file(file: &File) -> Result<(), SecretStoreError> {
    let metadata = file.metadata().map_err(io_error)?;
    reject_link(&metadata)?;
    if !metadata.is_file() {
        return Err(SecretStoreError::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(SecretStoreError::Invalid);
        }
    }
    Ok(())
}

fn no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    #[cfg(not(any(unix, windows)))]
    let _ = options;
}

#[cfg(unix)]
fn owner_only(file: &File) -> Result<(), SecretStoreError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().map_err(io_error)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(SecretStoreError::Denied);
    }
    Ok(())
}

#[cfg(windows)]
fn owner_only(file: &File) -> Result<(), SecretStoreError> {
    vulcan_winacl::verify_private_file(file).map_err(io_error)
}

#[cfg(not(any(unix, windows)))]
fn owner_only(_file: &File) -> Result<(), SecretStoreError> {
    Err(SecretStoreError::Unsupported)
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> Result<(), SecretStoreError> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(io_error)
}

#[cfg(windows)]
fn create_private_directory(path: &Path) -> Result<(), SecretStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(()), // The caller reopens and validates the actual directory.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(SecretStoreError::Invalid)?;
            create_private_directory(parent)?;
            match vulcan_winacl::create_private_directory(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(io_error(error)),
            }
        }
        Err(error) => Err(io_error(error)),
    }
}

#[cfg(not(any(unix, windows)))]
fn create_private_directory(_path: &Path) -> Result<(), SecretStoreError> {
    Err(SecretStoreError::Unsupported)
}

// Keep one fallible publication boundary across platforms; std has no portable
// directory-flush operation on Windows. The staged file is flushed above.
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn sync_directory(directory: &File) -> io::Result<()> {
    #[cfg(unix)]
    return directory.sync_all();
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> SecretName {
        SecretName::parse("mcp-issuer").unwrap()
    }

    #[test]
    fn inspection_is_state_free_and_missing_is_not_an_empty_secret() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("secrets");
        let store = ProtectedFileSecretStore::at(&root);
        assert_eq!(store.inspect(&name()), SecretStoreState::Missing);
        assert_eq!(store.get(&name()).unwrap_err(), SecretStoreError::Missing);
        assert_eq!(store.delete(&name()), Err(SecretStoreError::Missing));
        assert!(!root.exists());
        assert_eq!(
            store.capabilities().unattended,
            CapabilitySupport::Supported
        );
        assert!(!format!("{store:?}").contains(&root.display().to_string()));
        store.prepare_directory().unwrap();
        assert_eq!(store.inspect(&name()), SecretStoreState::Missing);
        assert_eq!(fs::read_dir(root).unwrap().count(), 0);
    }

    #[test]
    fn create_is_durable_non_replacing_and_delete_is_explicit() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("secrets");
        let store = ProtectedFileSecretStore::at(&root);
        let value = SecretBytes::new(b"original credential".to_vec()).unwrap();
        store.create(&name(), &value).unwrap();
        assert_eq!(store.inspect(&name()), SecretStoreState::Available);
        let restarted = ProtectedFileSecretStore::at(root);
        assert_eq!(restarted.get(&name()).unwrap().expose(), value.expose());
        assert_eq!(
            restarted.create(&name(), &SecretBytes::new(b"replacement".to_vec()).unwrap()),
            Err(SecretStoreError::AlreadyExists)
        );
        assert_eq!(store.get(&name()).unwrap().expose(), value.expose());
        restarted.delete(&name()).unwrap();
        assert_eq!(store.inspect(&name()), SecretStoreState::Missing);
    }

    #[test]
    fn concurrent_creates_publish_exactly_one_complete_value() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ProtectedFileSecretStore::at(temporary.path().join("secrets"));
        store.prepare_directory().unwrap();
        let threads: Vec<_> = (1..=8)
            .map(|byte| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store.create(
                        &name(),
                        &SecretBytes::new(vec![byte; MAX_SECRET_BYTES]).unwrap(),
                    )
                })
            })
            .collect();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(results.iter().all(|result| matches!(
            result,
            Ok(()) | Err(SecretStoreError::Locked | SecretStoreError::AlreadyExists)
        )));
        let value = store.get(&name()).unwrap();
        assert_eq!(value.expose().len(), MAX_SECRET_BYTES);
        assert!(value.expose().iter().all(|byte| *byte == value.expose()[0]));
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_links_and_invalid_files_fail_closed_without_repair() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("secrets");
        let store = ProtectedFileSecretStore::at(&root);
        store
            .create(&name(), &SecretBytes::new(vec![42]).unwrap())
            .unwrap();
        let path = store.path(&name());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(store.inspect(&name()), SecretStoreState::Denied);
        assert_eq!(store.get(&name()).unwrap_err(), SecretStoreError::Denied);
        assert_eq!(store.delete(&name()), Err(SecretStoreError::Denied));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::remove_file(&path).unwrap();
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"outside secret").unwrap();
        symlink(&outside, &path).unwrap();
        assert_eq!(store.get(&name()).unwrap_err(), SecretStoreError::Invalid);
        assert_eq!(store.delete(&name()), Err(SecretStoreError::Invalid));
        assert_eq!(fs::read(&outside).unwrap(), b"outside secret");
        fs::remove_file(&path).unwrap();
        fs::hard_link(&outside, &path).unwrap();
        assert_eq!(store.get(&name()).unwrap_err(), SecretStoreError::Invalid);
        fs::remove_file(&path).unwrap();
        File::create(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(store.inspect(&name()), SecretStoreState::Invalid);
        fs::write(&path, vec![1; MAX_SECRET_BYTES + 1]).unwrap();
        assert_eq!(store.get(&name()).unwrap_err(), SecretStoreError::Invalid);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(store.inspect(&name()), SecretStoreState::Denied);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_store_roots_are_not_followed() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let root = temporary.path().join("secrets");
        symlink(&outside, &root).unwrap();
        let store = ProtectedFileSecretStore::at(root);
        assert_eq!(store.inspect(&name()), SecretStoreState::Invalid);
        assert_eq!(
            store.create(&name(), &SecretBytes::new(vec![1]).unwrap()),
            Err(SecretStoreError::Invalid)
        );
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_secrets_are_refused_when_symlink_creation_is_available() {
        use std::os::windows::fs::symlink_file;
        let temporary = tempfile::tempdir().unwrap();
        let store = ProtectedFileSecretStore::at(temporary.path().join("secrets"));
        store.prepare_directory().unwrap();
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"outside credential").unwrap();
        // Windows may require Developer Mode or a privileged account to create
        // symlinks; ordinary file/ACL conformance remains unconditional above.
        if symlink_file(&outside, store.path(&name())).is_err() {
            return;
        }
        assert_eq!(store.get(&name()).unwrap_err(), SecretStoreError::Invalid);
        assert_eq!(store.delete(&name()), Err(SecretStoreError::Invalid));
        assert_eq!(fs::read(outside).unwrap(), b"outside credential");
    }
}
