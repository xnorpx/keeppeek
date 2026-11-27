//! Fail-closed filesystem ACL validation for supervisor root directories.
//!
//! Because a supervisor executes binaries from `<root>/versions` and
//! `<root>/vision` and honors control/readiness files under `<root>/runtime`,
//! write access to the supervisor root is equivalent to code execution under
//! the supervisor's OS account. Elevated hosts (such as Windows `LocalSystem`)
//! and `--allow-local` hosts validate directory permissions before launching.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Failure returned when a supervisor directory does not satisfy ownership or write-ACL bounds.
#[derive(Debug)]
pub enum AclError {
    /// Target path is a symbolic link or reparse point rather than a real directory.
    SymlinkNotAllowed { path: PathBuf },
    /// Directory grants write permissions to untrusted or overly broad principals.
    InsecurePermissions { path: PathBuf, detail: String },
    /// Querying OS metadata or security descriptors failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for AclError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SymlinkNotAllowed { path } => write!(
                formatter,
                "supervisor directory {} must not be a symbolic link or reparse point",
                path.display()
            ),
            Self::InsecurePermissions { path, detail } => write!(
                formatter,
                "supervisor directory {} has insecure permissions: {detail}",
                path.display()
            ),
            Self::Io { path, source } => write!(
                formatter,
                "unable to verify permissions on supervisor directory {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for AclError {}

/// Validates that `root` and any existing supervisor subdirectories reject unprivileged writers.
///
/// # Errors
///
/// Returns [`AclError`] if `root` or an existing `versions`, `vision`, or `runtime`
/// subdirectory is a symlink, owned by an unexpected principal, or writable by
/// group/world (Unix) or unprivileged broad groups such as Users/Everyone (Windows).
pub fn verify_supervisor_root_acl(root: &Path) -> Result<(), AclError> {
    const MAX_CHECKED_SLOT_DIRS: usize = 64;

    if !root.exists() {
        return Ok(());
    }
    verify_single_dir(root)?;
    for sub in ["versions", "vision", "runtime"] {
        let child = root.join(sub);
        if child.exists() {
            verify_single_dir(&child)?;
            if matches!(sub, "versions" | "vision")
                && let Ok(entries) = fs::read_dir(&child)
            {
                for entry in entries.flatten().take(MAX_CHECKED_SLOT_DIRS) {
                    let path = entry.path();
                    if path.is_dir() {
                        verify_single_dir(&path)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Ensures `dir` exists and applies owner-only permissions (`0o700` on Unix, protected
/// owner/SYSTEM/Administrators DACL on Windows) to every newly created directory component.
///
/// # Errors
///
/// Returns an I/O error if directory creation or Windows DACL tightening fails.
pub fn create_private_dir_all(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(dir)
    }
    #[cfg(windows)]
    {
        windows_acl::create_private_dir_all_windows(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        fs::create_dir_all(dir)
    }
}

fn verify_single_dir(path: &Path) -> Result<(), AclError> {
    let meta = fs::symlink_metadata(path).map_err(|source| AclError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if meta.file_type().is_symlink() {
        return Err(AclError::SymlinkNotAllowed {
            path: path.to_path_buf(),
        });
    }
    if !meta.is_dir() {
        return Err(AclError::InsecurePermissions {
            path: path.to_path_buf(),
            detail: "path is not a directory".to_owned(),
        });
    }
    verify_platform_dir_acl(path, &meta)
}

#[cfg(unix)]
fn verify_platform_dir_acl(path: &Path, meta: &fs::Metadata) -> Result<(), AclError> {
    use std::os::unix::fs::MetadataExt;

    let current_uid = rustix::process::geteuid().as_raw();
    let owner_uid = meta.uid();
    if owner_uid != current_uid && owner_uid != 0 {
        return Err(AclError::InsecurePermissions {
            path: path.to_path_buf(),
            detail: format!("owned by uid {owner_uid}, expected uid {current_uid} or root"),
        });
    }
    let mode = meta.mode();
    if mode & 0o022 != 0 {
        return Err(AclError::InsecurePermissions {
            path: path.to_path_buf(),
            detail: format!(
                "mode {:04o} allows group or world write access; restrict to owner-only (0700)",
                mode & 0o7777
            ),
        });
    }
    Ok(())
}

#[cfg(windows)]
fn verify_platform_dir_acl(path: &Path, _meta: &fs::Metadata) -> Result<(), AclError> {
    windows_acl::verify_directory_dacl(path)
}

#[cfg(windows)]
mod windows_acl {
    use std::mem::{self, MaybeUninit};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::ptr::{self, NonNull};

    use windows::Win32::Foundation::{GENERIC_ALL, GENERIC_WRITE, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::{
        self,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
        },
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, DELETE, FILE_APPEND_DATA, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, OPEN_EXISTING, READ_CONTROL,
        WRITE_DAC, WRITE_OWNER,
    };
    use windows::Win32::System::SystemServices::{
        ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, ACCESS_DENIED_CALLBACK_ACE_TYPE,
        ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE, ACCESS_DENIED_OBJECT_ACE_TYPE,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{BOOL, PCWSTR, PWSTR};

    use super::AclError;

    const MAX_CHECKED_ACES: u32 = 256;
    const INHERIT_ONLY_ACE_FLAG: u8 = 0x08;
    const CREATOR_OWNER_SID: &str = "S-1-3-0";
    const TRUSTED_SYSTEM_OWNERS: [&str; 2] = [
        "S-1-5-18",     // LocalSystem
        "S-1-5-32-544", // Builtin Administrators
    ];
    const TRUSTED_WRITE_SYSTEM_SIDS: [&str; 3] = [
        "S-1-5-18",                                                       // LocalSystem
        "S-1-5-32-544",                                                   // Builtin Administrators
        "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464", // TrustedInstaller
    ];
    const WRITE_ACCESS_MASK: u32 = FILE_WRITE_DATA.0
        | FILE_APPEND_DATA.0
        | FILE_WRITE_EA.0
        | FILE_WRITE_ATTRIBUTES.0
        | DELETE.0
        | WRITE_DAC.0
        | WRITE_OWNER.0
        | GENERIC_WRITE.0
        | GENERIC_ALL.0;

    struct LocalAllocation(*mut std::ffi::c_void);

    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            // SAFETY: Allocation came from Windows LocalAlloc-backed security APIs.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0)));
            }
        }
    }

    pub(super) fn create_private_dir_all_windows(dir: &Path) -> std::io::Result<()> {
        if dir.exists() {
            return Ok(());
        }
        // If the nearest existing ancestor already enforces a valid locked-down DACL
        // (for example a LocalSystem-owned supervisor root with protected SY+BA OICI
        // inheritance), inherit it untouched so an elevated administrator running
        // `keeppeek-supervisor update` never stamps their user SID onto slot directories.
        let mut ancestor = dir.parent();
        let mut parent_is_private = false;
        while let Some(path) = ancestor {
            if path.as_os_str().is_empty() {
                break;
            }
            if path.exists() {
                parent_is_private = verify_directory_dacl(path).is_ok();
                break;
            }
            ancestor = path.parent();
        }
        std::fs::create_dir_all(dir)?;
        if !parent_is_private {
            let current_sid = current_user_sid(dir).map_err(std::io::Error::other)?;
            let sddl = if TRUSTED_SYSTEM_OWNERS.contains(&current_sid.as_str()) {
                "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)".to_owned()
            } else {
                format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{current_sid})")
            };
            apply_sddl_dacl(dir, &sddl)?;
        }
        Ok(())
    }

    pub(super) fn apply_sddl_dacl(path: &Path, sddl: &str) -> std::io::Result<()> {
        let wide_sddl: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd = Security::PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide_sddl` is NUL-terminated; `sd` receives a LocalAlloc security descriptor.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide_sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .map_err(std::io::Error::other)?;
        let _allocation = LocalAllocation(sd.0);

        let mut present = BOOL::default();
        let mut defaulted = BOOL::default();
        let mut dacl = MaybeUninit::<*mut Security::ACL>::zeroed();
        // SAFETY: `sd` is valid and owned by `_allocation`.
        unsafe {
            Security::GetSecurityDescriptorDacl(sd, &mut present, dacl.as_mut_ptr(), &mut defaulted)
        }
        .map_err(std::io::Error::other)?;
        if !present.as_bool() {
            return Err(std::io::Error::other(
                "SDDL security descriptor lacked a DACL",
            ));
        }
        // SAFETY: `GetSecurityDescriptorDacl` initialized `dacl`.
        let raw_dacl = unsafe { dacl.assume_init() };
        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide_path` is NUL-terminated and `raw_dacl` lives inside `_allocation`.
        unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(wide_path.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(raw_dacl),
                None,
            )
        }
        .ok()
        .map_err(std::io::Error::other)
    }

    pub(super) fn verify_directory_dacl(path: &Path) -> Result<(), AclError> {
        let current_sid = current_user_sid(path)?;
        verify_directory_dacl_as(path, &current_sid)
    }

    pub(super) fn verify_directory_dacl_as(path: &Path, current_sid: &str) -> Result<(), AclError> {
        let dir_handle = open_directory_read_control(path)?;
        let descriptor = query_security_descriptor(path, &dir_handle)?;
        let owner_sid = verify_owner(path, &descriptor, current_sid)?;
        let dacl = extract_dacl(path, &descriptor)?;
        verify_dacl_aces(path, dacl, current_sid, &owner_sid)
    }

    #[cfg(test)]
    pub(super) fn is_dacl_protected(path: &Path) -> Result<bool, AclError> {
        let dir_handle = open_directory_read_control(path)?;
        let descriptor = query_security_descriptor(path, &dir_handle)?;
        let mut control: u16 = 0;
        let mut revision = 0_u32;
        // SAFETY: `descriptor.0` is a valid security descriptor owned by `descriptor`.
        unsafe {
            Security::GetSecurityDescriptorControl(
                Security::PSECURITY_DESCRIPTOR(descriptor.0),
                &raw mut control,
                &raw mut revision,
            )
        }
        .map_err(|error| AclError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        Ok((control & Security::SE_DACL_PROTECTED.0) != 0)
    }

    fn open_directory_read_control(path: &Path) -> Result<OwnedHandle, AclError> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is NUL-terminated and lives across `CreateFileW`.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                READ_CONTROL.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(|error| AclError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        // SAFETY: `CreateFileW` returned a fresh owned OS handle closed once by `OwnedHandle`.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
    }

    fn query_security_descriptor(
        path: &Path,
        handle: &OwnedHandle,
    ) -> Result<LocalAllocation, AclError> {
        let mut descriptor = Security::PSECURITY_DESCRIPTOR::default();
        // SAFETY: `handle` is live and `descriptor` receives a LocalAlloc security descriptor.
        unsafe {
            GetSecurityInfo(
                HANDLE(handle.as_raw_handle()),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&mut descriptor),
            )
        }
        .ok()
        .map_err(|error| AclError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        if descriptor.0.is_null() {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: "Windows returned a null security descriptor".to_owned(),
            });
        }
        Ok(LocalAllocation(descriptor.0))
    }

    fn verify_owner(
        path: &Path,
        descriptor: &LocalAllocation,
        current_sid: &str,
    ) -> Result<String, AclError> {
        let mut owner = Security::PSID::default();
        let mut defaulted = BOOL::default();
        // SAFETY: `descriptor.0` is a valid security descriptor owned by `descriptor`.
        unsafe {
            Security::GetSecurityDescriptorOwner(
                Security::PSECURITY_DESCRIPTOR(descriptor.0),
                &mut owner,
                &mut defaulted,
            )
        }
        .map_err(|error| AclError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        let owner_sid = sid_to_string(path, owner)?;
        let trusted =
            owner_sid == current_sid || TRUSTED_SYSTEM_OWNERS.contains(&owner_sid.as_str());
        if !trusted {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: format!(
                    "owner SID `{owner_sid}` is not the current user, Administrators, or LocalSystem"
                ),
            });
        }
        Ok(owner_sid)
    }

    fn extract_dacl(
        path: &Path,
        descriptor: &LocalAllocation,
    ) -> Result<NonNull<Security::ACL>, AclError> {
        let mut present = BOOL::default();
        let mut defaulted = BOOL::default();
        let mut acl = MaybeUninit::<*mut Security::ACL>::zeroed();
        // SAFETY: `descriptor.0` is a live security descriptor; `acl` receives a borrowed pointer.
        unsafe {
            Security::GetSecurityDescriptorDacl(
                Security::PSECURITY_DESCRIPTOR(descriptor.0),
                &mut present,
                acl.as_mut_ptr(),
                &mut defaulted,
            )
        }
        .map_err(|error| AclError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        if !present.as_bool() {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: "missing DACL grants unrestricted access".to_owned(),
            });
        }
        // SAFETY: `GetSecurityDescriptorDacl` initialized `acl`.
        let raw_acl = unsafe { acl.assume_init() };
        let Some(pointer) = NonNull::new(raw_acl) else {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: "null DACL grants Everyone full write access".to_owned(),
            });
        };
        // SAFETY: `pointer` points to a non-null ACL inside `descriptor`.
        if !unsafe { Security::IsValidAcl(pointer.as_ptr()) }.as_bool() {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: "invalid Windows DACL".to_owned(),
            });
        }
        Ok(pointer)
    }

    fn verify_dacl_aces(
        path: &Path,
        acl: NonNull<Security::ACL>,
        current_sid: &str,
        owner_sid: &str,
    ) -> Result<(), AclError> {
        let mut info = Security::ACL_SIZE_INFORMATION::default();
        let info_size = u32::try_from(mem::size_of::<Security::ACL_SIZE_INFORMATION>())
            .expect("ACL_SIZE_INFORMATION fits u32");
        // SAFETY: `acl` is validated and `info` has full struct size.
        unsafe {
            Security::GetAclInformation(
                acl.as_ptr(),
                ptr::from_mut(&mut info).cast(),
                info_size,
                Security::AclSizeInformation,
            )
        }
        .map_err(|error| AclError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        if info.AceCount > MAX_CHECKED_ACES {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: format!(
                    "DACL contains {} ACEs, exceeding limit {MAX_CHECKED_ACES}",
                    info.AceCount
                ),
            });
        }
        for index in 0..info.AceCount {
            check_single_ace(path, acl, index, current_sid, owner_sid)?;
        }
        Ok(())
    }

    fn check_single_ace(
        path: &Path,
        acl: NonNull<Security::ACL>,
        index: u32,
        current_sid: &str,
        owner_sid: &str,
    ) -> Result<(), AclError> {
        let mut raw = MaybeUninit::<*mut std::ffi::c_void>::zeroed();
        // SAFETY: `index < AceCount` for validated `acl`.
        unsafe { Security::GetAce(acl.as_ptr(), index, raw.as_mut_ptr()) }.map_err(|error| {
            AclError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other(error),
            }
        })?;
        // SAFETY: `GetAce` succeeded and initialized `raw`.
        let ace_ptr = unsafe { raw.assume_init() };
        if ace_ptr.is_null() {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: "null ACE entry in Windows DACL".to_owned(),
            });
        }
        // SAFETY: Non-null pointer from `GetAce` starts with `ACE_HEADER`.
        let header = unsafe { ace_ptr.cast::<Security::ACE_HEADER>().read_unaligned() };
        let ace_type = u32::from(header.AceType);
        if matches!(
            ace_type,
            ACCESS_DENIED_ACE_TYPE
                | ACCESS_DENIED_OBJECT_ACE_TYPE
                | ACCESS_DENIED_CALLBACK_ACE_TYPE
                | ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE
        ) {
            return Ok(());
        }
        if ace_type != ACCESS_ALLOWED_ACE_TYPE {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: format!(
                    "unsupported or conditional ACE type 0x{ace_type:02x} rejected fail-closed"
                ),
            });
        }
        let sid_offset = mem::offset_of!(Security::ACCESS_ALLOWED_ACE, SidStart);
        let sid_header = mem::offset_of!(Security::SID, SubAuthority);
        if usize::from(header.AceSize) < sid_offset + sid_header {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: "malformed ACCESS_ALLOWED_ACE size".to_owned(),
            });
        }
        // SAFETY: Verified ACE size covers `Mask` and `SID` header inside the live ACL buffer.
        let (mask, sid) = unsafe {
            let mask = ace_ptr
                .byte_add(mem::offset_of!(Security::ACCESS_ALLOWED_ACE, Mask))
                .cast::<u32>()
                .read_unaligned();
            let sid = Security::PSID(ace_ptr.byte_add(sid_offset));
            (mask, sid)
        };
        if mask & WRITE_ACCESS_MASK == 0 {
            return Ok(());
        }
        let sid_str = sid_to_string(path, sid)?;
        let is_inherit_only_creator_owner =
            sid_str == CREATOR_OWNER_SID && (header.AceFlags & INHERIT_ONLY_ACE_FLAG) != 0;
        let allowed = sid_str == current_sid
            || sid_str == owner_sid
            || TRUSTED_WRITE_SYSTEM_SIDS.contains(&sid_str.as_str())
            || is_inherit_only_creator_owner;
        if !allowed {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: format!(
                    "grants write access (mask 0x{mask:08x}) to non-allowlisted principal `{sid_str}`"
                ),
            });
        }
        Ok(())
    }

    pub(super) fn current_user_sid(path: &Path) -> Result<String, AclError> {
        let mut token = HANDLE::default();
        // SAFETY: Queries current process token with read-only `TOKEN_QUERY`.
        unsafe { OpenProcessToken(GetCurrentProcess(), Security::TOKEN_QUERY, &mut token) }
            .map_err(|error| AclError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::other(error),
            })?;
        // SAFETY: `OpenProcessToken` returned an owned token handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
        let mut buffer = [0_usize; 128];
        let mut written = 0_u32;
        let buffer_size = u32::try_from(mem::size_of_val(&buffer)).expect("buffer fits u32");
        // SAFETY: `buffer` is aligned and sized beyond `TOKEN_USER` + max SID length.
        unsafe {
            Security::GetTokenInformation(
                HANDLE(token.as_raw_handle()),
                Security::TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                buffer_size,
                &mut written,
            )
        }
        .map_err(|error| AclError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        if usize::try_from(written).unwrap_or(0) < mem::size_of::<Security::TOKEN_USER>() {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: "incomplete Windows TokenUser response".to_owned(),
            });
        }
        // SAFETY: `GetTokenInformation(TokenUser)` initialized `TOKEN_USER` at buffer start.
        let sid = unsafe { (*buffer.as_ptr().cast::<Security::TOKEN_USER>()).User.Sid };
        sid_to_string(path, sid)
    }

    fn sid_to_string(path: &Path, sid: Security::PSID) -> Result<String, AclError> {
        // SAFETY: `IsValidSid` validates the SID before formatting.
        if sid.0.is_null() || !unsafe { Security::IsValidSid(sid) }.as_bool() {
            return Err(AclError::InsecurePermissions {
                path: path.to_path_buf(),
                detail: "invalid Windows SID in directory security descriptor".to_owned(),
            });
        }
        let mut text = PWSTR::null();
        // SAFETY: `sid` was validated and `text` receives a LocalAlloc NUL-terminated UTF-16 string.
        unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(|error| AclError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
        let _allocation = LocalAllocation(text.0.cast());
        // SAFETY: `ConvertSidToStringSidW` populated a valid NUL-terminated UTF-16 string.
        unsafe { text.to_string() }.map_err(|error| AclError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(error),
        })
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        create_private_dir_all, verify_supervisor_root_acl,
        windows_acl::{
            apply_sddl_dacl, current_user_sid, is_dacl_protected, verify_directory_dacl_as,
        },
    };

    #[test]
    fn windows_acl_allowlist_protects_new_dirs_and_rejects_broad_writers() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let base =
            std::env::temp_dir().join(format!("keeppeek-win-acl-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let current_sid = current_user_sid(&base).unwrap();

        // Simulate a permissive parent (like C:\ProgramData where BUILTIN\Users has write access).
        let permissive_parent = base.join("programdata-like");
        fs::create_dir_all(&permissive_parent).unwrap();
        let permissive_sddl = format!(
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{current_sid})(A;OICI;FA;;;BU)"
        );
        apply_sddl_dacl(&permissive_parent, &permissive_sddl).unwrap();
        assert!(verify_supervisor_root_acl(&permissive_parent).is_err());

        // Creating a supervisor root inside a permissive parent locks down ONLY the root itself
        // (leaving the permissive parent untouched).
        let root = permissive_parent.join("keeppeek").join("supervisor");
        create_private_dir_all(&root).expect("private dir creation must succeed");
        verify_supervisor_root_acl(&root).expect("protected owner-only root must pass ACL check");
        assert!(is_dacl_protected(&root).unwrap());
        assert!(
            verify_supervisor_root_acl(&permissive_parent).is_err(),
            "unrelated parent directory permissions must not be modified"
        );

        // Creating slot directories inside an already-private root inherits the root's DACL
        // untouched (no SE_DACL_PROTECTED re-stamping with a caller's personal user SID).
        let staging = root.join("versions").join(".staging-1");
        create_private_dir_all(&staging).expect("nested private dir creation must succeed");
        assert!(
            !is_dacl_protected(&staging).unwrap(),
            "subdirectories inside a private root must inherit its protected OICI DACL"
        );
        verify_supervisor_root_acl(&root).expect("root with inherited slot dir must pass");

        // Verify simulated SYSTEM (S-1-5-18) service root behavior: a directory with SY+BA only
        // passes SYSTEM verification, and if the current user can create a child inside it
        // (elevated admin), the child inherits SY+BA and ALSO passes SYSTEM verification.
        let system_root = base.join("system-like");
        fs::create_dir_all(&system_root).unwrap();
        if apply_sddl_dacl(&system_root, "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)").is_ok() {
            // Note: on a non-elevated account, owner is current_sid so verify_owner with SYSTEM
            // requires owner == SY/BA; test the elevated admin child-inheritance path when writable.
            let slot_child = system_root.join("versions").join("slot-1");
            if create_private_dir_all(&slot_child).is_ok() {
                assert!(!is_dacl_protected(&slot_child).unwrap());
                assert!(
                    verify_directory_dacl_as(&slot_child, "S-1-5-18").is_ok(),
                    "elevated admin slot dir inside SY+BA root must remain valid for LocalSystem"
                );
            }
            // Restore current user FA so cleanup succeeds cleanly even when non-elevated.
            let cleanup_sddl =
                format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{current_sid})");
            let _ = apply_sddl_dacl(&system_root, &cleanup_sddl);
        }

        // Granting write access to BUILTIN\Users (BU) must fail closed.
        apply_sddl_dacl(&root, &permissive_sddl).unwrap();
        assert!(
            verify_supervisor_root_acl(&root).is_err(),
            "BUILTIN\\Users write ACE must be rejected fail-closed"
        );

        // Granting write access to Everyone (WD) must fail closed.
        let everyone_sddl = format!(
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{current_sid})(A;OICI;FA;;;WD)"
        );
        apply_sddl_dacl(&root, &everyone_sddl).unwrap();
        assert!(
            verify_supervisor_root_acl(&root).is_err(),
            "Everyone write ACE must be rejected fail-closed"
        );

        // Read-only ACE for BUILTIN\Users (no write bits) alongside current user/SYSTEM/Administrators is safe.
        let ro_sddl = format!(
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{current_sid})(A;OICI;FR;;;BU)"
        );
        apply_sddl_dacl(&root, &ro_sddl).unwrap();
        verify_supervisor_root_acl(&root)
            .expect("read-only ACE for BUILTIN\\Users without write bits must pass");
        let _ = fs::remove_dir_all(&base);
    }
}
