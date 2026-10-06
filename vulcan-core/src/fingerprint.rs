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
