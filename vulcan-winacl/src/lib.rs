//! Windows private-path ACL creation, verification, and repair.
//!
//! This is the only Vulcan crate that needs `unsafe`: it wraps the Win32 security APIs behind a
//! small safe interface so `vulcan-app` and `vulcan-daemon` keep `forbid(unsafe_code)`.
//!
//! Unix expresses "private" as mode bits; Windows expresses it as a DACL. A path is private
//! here when its owner is the current user (or the Administrators group, which owns objects
//! created by an elevated token) and every effective access-allowed entry that grants more than
//! attribute/metadata reads names only the current user, `SYSTEM`, or `BUILTIN\Administrators`.
//! A missing (NULL) DACL means "everyone", so it is rejected. Unrecognized ACE kinds fail
//! closed. Deny entries are ignored because they can only narrow access.
//!
//! New identity directories are created with a protected DACL (no inheritance from the parent)
//! that grants full control to the current user and `SYSTEM` and is inherited by children.

#![cfg(windows)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_SUCCESS, FALSE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    AclSizeInformation, CreateWellKnownSid, EqualSid, GetAce, GetAclInformation, GetLengthSid,
    GetSecurityDescriptorDacl, GetTokenInformation, IsValidSid, TokenUser,
    WinBuiltinAdministratorsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
    ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_READ_EA, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    READ_CONTROL, SYNCHRONIZE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
/// Types that cannot grant access to the object (audit and mandatory-label entries).
const SYSTEM_AUDIT_ACE_TYPE: u8 = 2;
const SYSTEM_ALARM_ACE_TYPE: u8 = 3;
const SYSTEM_MANDATORY_LABEL_ACE_TYPE: u8 = 0x11;
const INHERIT_ONLY_ACE: u8 = 0x08;
/// Rights an untrusted principal may hold without exposing key material or allowing tampering.
const HARMLESS_RIGHTS: u32 = SYNCHRONIZE | READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_READ_EA;

/// Owned copy of a SID's bytes.
#[derive(Clone)]
struct Sid(Vec<u8>);

impl Sid {
    fn as_ptr(&self) -> *mut c_void {
        self.0.as_ptr().cast_mut().cast()
    }

    fn equals(&self, other: *mut c_void) -> bool {
        // SAFETY: both pointers name valid SIDs; `other` comes from an ACL/descriptor we hold.
        unsafe { IsValidSid(other) != FALSE && EqualSid(self.as_ptr(), other) != FALSE }
    }

    fn well_known(kind: i32) -> io::Result<Self> {
        let mut size = 0_u32;
        // SAFETY: a null output buffer with a zero size only queries the required length.
        unsafe { CreateWellKnownSid(kind, null_mut(), null_mut(), &raw mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut bytes = vec![0_u8; size as usize];
        // SAFETY: `bytes` has the size CreateWellKnownSid requested.
        let ok = unsafe {
            CreateWellKnownSid(kind, null_mut(), bytes.as_mut_ptr().cast(), &raw mut size)
        };
        if ok == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(bytes))
    }

    fn to_sddl(&self) -> io::Result<String> {
        let mut text: *mut u16 = null_mut();
        // SAFETY: `self` is a valid SID; on success `text` is a LocalAlloc'd NUL-terminated string.
        if unsafe { ConvertSidToStringSidW(self.as_ptr(), &raw mut text) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        let mut length = 0;
        // SAFETY: the string is NUL-terminated.
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: `length` code units were just observed to be readable.
        let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
        // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc.
        unsafe { LocalFree(text.cast()) };
        Ok(value)
    }
}

fn current_user_sid() -> io::Result<Sid> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo-handle from GetCurrentProcess is always valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    let result = token_user_sid(token);
    // SAFETY: `token` was opened above.
    unsafe { CloseHandle(token) };
    result
}

fn token_user_sid(token: HANDLE) -> io::Result<Sid> {
    let mut needed = 0_u32;
    // SAFETY: a null buffer only queries the required size.
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &raw mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 backing keeps the buffer aligned for TOKEN_USER.
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(8)];
    // SAFETY: the buffer is at least `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    };
    if ok == FALSE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID lives inside it.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    // SAFETY: `sid` is a valid SID inside `buffer`.
    let length = unsafe { GetLengthSid(sid) } as usize;
    // SAFETY: GetLengthSid reported this many readable bytes.
    Ok(Sid(unsafe {
        std::slice::from_raw_parts(sid.cast::<u8>(), length)
    }
    .to_vec()))
}

struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the descriptor was allocated by the security API with LocalAlloc.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    units.push(0);
    Ok(units)
}

/// Build a self-relative descriptor from SDDL text. The result owns `LocalAlloc`'d memory.
fn descriptor_from_sddl(sddl: &str) -> io::Result<LocalMemory> {
    let sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor: *mut c_void = null_mut();
    // SAFETY: `sddl` is NUL-terminated; on success `descriptor` is LocalAlloc'd.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            null_mut(),
        )
    };
    if ok == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalMemory(descriptor))
}

/// Protected DACL text: full control for the current user and `SYSTEM` only. Directories
/// pass `inherit` so children receive the same private DACL.
fn private_sddl(inherit: bool) -> io::Result<String> {
    let user = current_user_sid()?.to_sddl()?;
    let flags = if inherit { "OICI" } else { "" };
    Ok(format!("D:PAI(A;{flags};FA;;;{user})(A;{flags};FA;;;SY)"))
}

/// Create one directory with a protected DACL: full control for the current user and `SYSTEM`
/// only, inherited by files and subdirectories. Fails if the directory already exists.
pub fn create_private_directory(path: &Path) -> io::Result<()> {
    let descriptor = descriptor_from_sddl(&private_sddl(true)?)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: FALSE,
    };
    let path = wide(path)?;
    // SAFETY: `path` is NUL-terminated and `attributes` outlives the call.
    if unsafe { CreateDirectoryW(path.as_ptr(), &raw const attributes) } == FALSE {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Replace the DACL of an existing file or directory owned by the current user with the
/// protected private DACL. It never changes ownership, so an object owned by someone else is
/// refused rather than taken over. Callers must already have rejected reparse points.
pub fn repair_private_path(path: &Path, directory: bool) -> io::Result<()> {
    let handle = open_for_security(path)?;
    verify_owner(&handle)
        .map_err(|reason| io::Error::new(io::ErrorKind::PermissionDenied, reason))?;
    let descriptor = descriptor_from_sddl(&private_sddl(directory)?)?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut ACL = null_mut();
    // SAFETY: `descriptor` is a valid descriptor built above.
    let ok = unsafe {
        GetSecurityDescriptorDacl(
            descriptor.0,
            &raw mut present,
            &raw mut dacl,
            &raw mut defaulted,
        )
    };
    if ok == FALSE || present == FALSE || dacl.is_null() {
        return Err(io::Error::other("cannot build the private DACL"));
    }
    let mut wide_path = wide(path)?;
    // SAFETY: `wide_path` is NUL-terminated and `dacl` lives inside `descriptor`.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    Ok(())
}

/// Open a handle that can read the security descriptor without following reparse points.
fn open_for_security(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(READ_CONTROL)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Verify that `path` (a file or directory) is private to the current user.
pub fn verify_private_path(path: &Path) -> io::Result<()> {
    let handle = open_for_security(path)?;
    verify_private_handle(&handle)
        .map_err(|reason| io::Error::new(io::ErrorKind::PermissionDenied, reason))
}

/// Verify an already-open file or directory handle, avoiding a path re-resolution race.
pub fn verify_private_file(file: &File) -> io::Result<()> {
    verify_private_handle(file)
        .map_err(|reason| io::Error::new(io::ErrorKind::PermissionDenied, reason))
}

/// Require that the object's owner is the current user, `Administrators` or `SYSTEM`.
fn verify_owner(file: &File) -> Result<(), &'static str> {
    let handle = file.as_raw_handle() as HANDLE;
    if handle == INVALID_HANDLE_VALUE {
        return Err("invalid file handle");
    }
    let user = current_user_sid().map_err(|_| "cannot determine the current user")?;
    let system = Sid::well_known(WinLocalSystemSid).map_err(|_| "cannot build the SYSTEM SID")?;
    let admins = Sid::well_known(WinBuiltinAdministratorsSid)
        .map_err(|_| "cannot build the Administrators SID")?;
    let mut owner: *mut c_void = null_mut();
    let mut descriptor: *mut c_void = null_mut();
    // SAFETY: `handle` is open with READ_CONTROL; outputs are valid pointers.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &raw mut descriptor,
        )
    };
    let _descriptor = LocalMemory(descriptor);
    if status != ERROR_SUCCESS {
        return Err("cannot read the security descriptor");
    }
    if owner.is_null() || !(user.equals(owner) || admins.equals(owner) || system.equals(owner)) {
        return Err("owner is not the current user");
    }
    Ok(())
}

fn verify_private_handle(file: &File) -> Result<(), &'static str> {
    let handle = file.as_raw_handle() as HANDLE;
    if handle == INVALID_HANDLE_VALUE {
        return Err("invalid file handle");
    }
    let user = current_user_sid().map_err(|_| "cannot determine the current user")?;
    let system = Sid::well_known(WinLocalSystemSid).map_err(|_| "cannot build the SYSTEM SID")?;
    let admins = Sid::well_known(WinBuiltinAdministratorsSid)
        .map_err(|_| "cannot build the Administrators SID")?;

    let mut owner: *mut c_void = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: *mut c_void = null_mut();
    // SAFETY: `handle` is open with READ_CONTROL; outputs are valid pointers.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            null_mut(),
            &raw mut dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    let _descriptor = LocalMemory(descriptor);
    if status != ERROR_SUCCESS {
        return Err("cannot read the security descriptor");
    }
    if owner.is_null() || !(user.equals(owner) || admins.equals(owner) || system.equals(owner)) {
        return Err("owner is not the current user");
    }
    if dacl.is_null() {
        return Err("has no DACL, which grants access to everyone");
    }

    let mut info = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };
    // SAFETY: `dacl` is a valid ACL and `info` has the size passed.
    let ok = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut info).cast(),
            u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(u32::MAX),
            AclSizeInformation,
        )
    };
    if ok == FALSE {
        return Err("cannot read the DACL");
    }
    for index in 0..info.AceCount {
        let mut ace: *mut c_void = null_mut();
        // SAFETY: `index` is below the ACE count reported for this ACL.
        if unsafe { GetAce(dacl, index, &raw mut ace) } == FALSE || ace.is_null() {
            return Err("cannot read a DACL entry");
        }
        // SAFETY: every ACE starts with an ACE_HEADER.
        let header = unsafe { *ace.cast::<ACE_HEADER>() };
        if header.AceFlags & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        match header.AceType {
            ACCESS_DENIED_ACE_TYPE
            | SYSTEM_AUDIT_ACE_TYPE
            | SYSTEM_ALARM_ACE_TYPE
            | SYSTEM_MANDATORY_LABEL_ACE_TYPE => continue,
            ACCESS_ALLOWED_ACE_TYPE => {}
            _ => return Err("has an unsupported access-control entry"),
        }
        // SAFETY: an ACCESS_ALLOWED_ACE_TYPE entry has the ACCESS_ALLOWED_ACE layout.
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = (&raw const allowed.SidStart).cast_mut().cast::<c_void>();
        if user.equals(sid) || system.equals(sid) || admins.equals(sid) {
            continue;
        }
        if allowed.Mask & !HARMLESS_RIGHTS != 0 {
            return Err("is accessible by users other than the current user");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn created_directory_and_inherited_children_are_private() {
        let root = tempfile::tempdir().expect("temporary directory");
        let private = root.path().join("private");
        create_private_directory(&private).expect("private directory");
        verify_private_path(&private).expect("directory is private");

        let child = private.join("secret");
        std::fs::write(&child, b"x").expect("child file");
        verify_private_path(&child).expect("child inherits the private DACL");
        let file = File::open(&child).expect("open child");
        verify_private_file(&file).expect("handle verification");

        assert_eq!(
            create_private_directory(&private)
                .expect_err("existing directory")
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn broad_grants_are_rejected() {
        let root = tempfile::tempdir().expect("temporary directory");
        let private = root.path().join("private");
        create_private_directory(&private).expect("private directory");
        let child = private.join("secret");
        std::fs::write(&child, b"x").expect("child file");

        let status = Command::new("icacls")
            .arg(&child)
            .args(["/grant", "*S-1-1-0:(R)"])
            .status()
            .expect("icacls");
        assert!(status.success());
        let error = verify_private_path(&child).expect_err("Everyone can read");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn repair_restores_a_private_dacl() {
        let root = tempfile::tempdir().expect("temporary directory");
        let path = root.path().join("loose");
        std::fs::write(&path, b"x").expect("file");
        let status = Command::new("icacls")
            .arg(&path)
            .args(["/grant", "*S-1-1-0:(R)"])
            .status()
            .expect("icacls");
        assert!(status.success());
        verify_private_path(&path).expect_err("loose ACL is rejected");
        repair_private_path(&path, false).expect("repair");
        verify_private_path(&path).expect("repaired ACL is private");
    }
}
