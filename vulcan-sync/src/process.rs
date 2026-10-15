//! Child-process helpers shared by the sync engines.

use std::io::ErrorKind;
use std::process::{Child, Command};
use std::time::Duration;

/// Spawn attempts before an executable that stays busy is reported.
const BUSY_ATTEMPTS: u32 = 6;

/// [`Command::spawn`], retrying while the executable is busy.
///
/// On Unix, executing a file that some process holds open for writing fails
/// with `ETXTBSY`. A freshly written script or wrapper hits this when another
/// thread forks while the script's write handle is still open: the child
/// inherits the handle until it calls `exec`, which takes microseconds. The
/// condition is transient, so the spawn is retried with a short backoff
/// (about 0.6 s in total) before the error is returned.
pub fn spawn_retrying_busy(command: &mut Command) -> std::io::Result<Child> {
    let mut delay = Duration::from_millis(10);
    let mut attempt = 1;
    loop {
        match command.spawn() {
            Err(error)
                if error.kind() == ErrorKind::ExecutableFileBusy && attempt < BUSY_ATTEMPTS =>
            {
                std::thread::sleep(delay);
                delay *= 2;
                attempt += 1;
            }
            result => return result,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_script_held_open_for_writing_runs_once_released() {
        let temporary = tempfile::tempdir().unwrap();
        let script = temporary.path().join("script");
        let mut file = std::fs::File::create(&script).unwrap();
        file.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        // While the write handle is open, the kernel refuses to execute it.
        assert_eq!(
            Command::new(&script).spawn().unwrap_err().kind(),
            ErrorKind::ExecutableFileBusy
        );
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(file);
        });
        let status = spawn_retrying_busy(&mut Command::new(&script))
            .unwrap()
            .wait()
            .unwrap();
        assert!(status.success());
        release.join().unwrap();
    }
}
