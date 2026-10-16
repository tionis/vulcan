//! Stat fingerprints: cheap evidence that a file is unchanged, shared by the
//! mdbase record cache and the note store's freshness proofs (QRY.4).

use std::fs;

/// Device, inode, size, and modification and status-change times in
/// nanoseconds, as big-endian 8-byte fields.
pub type StatFingerprint = [u8; 40];

/// Identity and change evidence for one regular file: device, inode, size,
/// modification time, and status-change time in nanoseconds. Unlike mtime,
/// ctime cannot be restored by ordinary tools, so a same-size edit with a
/// restored mtime still changes the fingerprint. Two writes within one
/// timestamp tick that keep the size and inode are indistinguishable; that
/// is the documented limit of stat-based proofs. Unavailable off Unix.
#[cfg(unix)]
#[must_use]
pub fn stat_fingerprint(metadata: &fs::Metadata) -> Option<StatFingerprint> {
    use std::os::unix::fs::MetadataExt;

    let nanoseconds = |seconds: i64, nanoseconds: i64| {
        seconds
            .saturating_mul(1_000_000_000)
            .saturating_add(nanoseconds)
    };
    metadata.is_file().then(|| {
        let mut fingerprint = [0; 40];
        let fields = [
            metadata.dev().to_be_bytes(),
            metadata.ino().to_be_bytes(),
            metadata.size().to_be_bytes(),
            nanoseconds(metadata.mtime(), metadata.mtime_nsec()).to_be_bytes(),
            nanoseconds(metadata.ctime(), metadata.ctime_nsec()).to_be_bytes(),
        ];
        for (chunk, field) in fingerprint.chunks_exact_mut(8).zip(fields) {
            chunk.copy_from_slice(&field);
        }
        fingerprint
    })
}

#[cfg(not(unix))]
#[must_use]
pub fn stat_fingerprint(_metadata: &fs::Metadata) -> Option<StatFingerprint> {
    None
}

/// The evidence an incremental scan trusts that a file is unchanged, as a
/// fingerprint the scan stores and the note store's freshness proof
/// compares. On Unix it is [`stat_fingerprint`]. On Windows, which has no
/// change time in directory metadata, it holds what the scan already
/// trusts there (size, last write time, and creation time) plus the file
/// attributes, at 100 ns resolution and under a tag of its own, so a proof
/// is exactly as strong as the scan it stands in for. A same-size edit that
/// restores the write time is not detected on Windows by either. The mdbase
/// record cache keeps [`stat_fingerprint`], which promises more.
#[must_use]
pub fn scan_fingerprint(metadata: &fs::Metadata) -> Option<StatFingerprint> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.is_file().then(|| {
            let mut fingerprint = [0; 40];
            let fields = [
                *b"WINSCAN1",
                metadata.creation_time().to_be_bytes(),
                metadata.file_size().to_be_bytes(),
                metadata.last_write_time().to_be_bytes(),
                u64::from(metadata.file_attributes()).to_be_bytes(),
            ];
            for (chunk, field) in fingerprint.chunks_exact_mut(8).zip(fields) {
                chunk.copy_from_slice(&field);
            }
            fingerprint
        })
    }
    #[cfg(not(windows))]
    {
        stat_fingerprint(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the platform records scan fingerprints, an unchanged file
    /// keeps its fingerprint and any change to its size or write time
    /// changes it.
    #[cfg(any(unix, windows))]
    #[test]
    fn scan_fingerprints_follow_size_and_write_time() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("Note.md");
        fs::write(&path, "one\n").unwrap();
        let first = scan_fingerprint(&fs::metadata(&path).unwrap()).expect("a fingerprint");
        assert_eq!(scan_fingerprint(&fs::metadata(&path).unwrap()), Some(first));
        fs::write(&path, "longer\n").unwrap();
        let resized = scan_fingerprint(&fs::metadata(&path).unwrap()).unwrap();
        assert_ne!(resized, first);
        let earlier =
            fs::metadata(&path).unwrap().modified().unwrap() - std::time::Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(earlier)
            .unwrap();
        assert_ne!(
            scan_fingerprint(&fs::metadata(&path).unwrap()).unwrap(),
            resized
        );
        assert_eq!(
            scan_fingerprint(&fs::metadata(temp_dir.path()).unwrap()),
            None
        );
    }
}
