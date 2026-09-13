//! Creates export files with a protected Windows DACL before writing private bytes.

use std::{
    fs::File,
    io, mem,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, Prefix},
};
use windows::{
    Win32::{
        Foundation::{GENERIC_WRITE, HANDLE, HLOCAL, LocalFree},
        Security::{
            self,
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
            },
        },
        Storage::FileSystem::{CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ},
        System::{
            SystemServices::ACCESS_ALLOWED_ACE_TYPE,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
    core::{BOOL, PCWSTR, PWSTR},
};

struct LocalAllocation(*mut std::ffi::c_void);

struct Dacl<'a> {
    pointer: *const Security::ACL,
    owner: &'a LocalAllocation,
}

struct AllowedAce<'a> {
    mask: u32,
    sid: Security::PSID,
    _owner: &'a LocalAllocation,
}

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // Windows allocated this descriptor or SID string with LocalAlloc.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

pub(super) fn create_private(path: &Path) -> io::Result<File> {
    let owner = current_user_sid()?;
    let descriptor = security_descriptor(&format!(
        "O:{owner}D:P(A;;FA;;;{owner})(A;;FA;;;SY)(A;;FA;;;BA)"
    ))?;
    let attributes = Security::SECURITY_ATTRIBUTES {
        nLength: u32::try_from(mem::size_of::<Security::SECURITY_ATTRIBUTES>()).unwrap(),
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let filename = wide_path(path)?;
    // Both buffers outlive CreateFileW; CREATE_NEW refuses existing files and reparse points.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(filename.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            Some(&attributes),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(io::Error::other)?;
    // A successful CreateFileW returns a fresh owned handle; File closes it exactly once.
    let file = unsafe { File::from_raw_handle(handle.0) };
    if let Err(error) = verify_file_security(&file, &descriptor) {
        drop(file);
        std::fs::remove_file(path)?;
        return Err(error);
    }
    Ok(file)
}

fn verify_file_security(file: &File, expected: &LocalAllocation) -> io::Result<()> {
    let mut descriptor = Security::PSECURITY_DESCRIPTOR::default();
    // The handle remains open; Windows allocates the returned descriptor and its nested data.
    unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            Security::OWNER_SECURITY_INFORMATION | Security::DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    }
    .ok()
    .map_err(io::Error::other)?;
    verify_descriptor(&LocalAllocation(descriptor.0), expected)
}

fn verify_descriptor(actual: &LocalAllocation, expected: &LocalAllocation) -> io::Result<()> {
    let mut control = 0;
    let mut revision = 0;
    // Both allocations contain descriptors returned by Windows and remain live throughout validation.
    unsafe {
        Security::GetSecurityDescriptorControl(
            Security::PSECURITY_DESCRIPTOR(actual.0),
            &mut control,
            &mut revision,
        )
    }
    .map_err(io::Error::other)?;
    if control & Security::SE_DACL_PROTECTED.0 == 0 {
        return Err(unverified_security());
    }
    let actual_owner = descriptor_owner(actual)?;
    let expected_owner = descriptor_owner(expected)?;
    // Both SIDs are validated pointers into the live descriptor allocations.
    unsafe { Security::EqualSid(actual_owner, expected_owner) }
        .map_err(|_| unverified_security())?;
    let actual_acl = descriptor_dacl(actual)?;
    let expected_acl = descriptor_dacl(expected)?;
    // These validated ACL headers remain within their borrowed descriptor allocations.
    let counts = unsafe {
        (
            (*actual_acl.pointer).AceCount,
            (*expected_acl.pointer).AceCount,
        )
    };
    if counts != (3, 3) {
        return Err(unverified_security());
    }
    let mut matched = [false; 3];
    for index in 0..3 {
        let actual = allowed_ace(&actual_acl, index)?;
        let mut found = false;
        for (candidate, matched) in matched.iter_mut().enumerate() {
            if !*matched && same_ace(&actual, &allowed_ace(&expected_acl, candidate as u32)?) {
                *matched = true;
                found = true;
                break;
            }
        }
        if !found {
            return Err(unverified_security());
        }
    }
    Ok(())
}

fn descriptor_owner(descriptor: &LocalAllocation) -> io::Result<Security::PSID> {
    let mut owner = Security::PSID::default();
    let mut defaulted = BOOL::default();
    // Windows returns a borrowed SID within this live security descriptor.
    unsafe {
        Security::GetSecurityDescriptorOwner(
            Security::PSECURITY_DESCRIPTOR(descriptor.0),
            &mut owner,
            &mut defaulted,
        )
    }
    .map_err(io::Error::other)?;
    // A non-null owner returned from a Windows descriptor can be checked by IsValidSid.
    if owner.0.is_null() || !unsafe { Security::IsValidSid(owner) }.as_bool() {
        return Err(unverified_security());
    }
    Ok(owner)
}

fn descriptor_dacl(descriptor: &LocalAllocation) -> io::Result<Dacl<'_>> {
    let mut present = BOOL::default();
    let mut defaulted = BOOL::default();
    let mut acl = std::ptr::null_mut();
    // Windows returns an ACL pointer borrowed from the descriptor, which outlives the result.
    unsafe {
        Security::GetSecurityDescriptorDacl(
            Security::PSECURITY_DESCRIPTOR(descriptor.0),
            &mut present,
            &mut acl,
            &mut defaulted,
        )
    }
    .map_err(io::Error::other)?;
    // A null or missing DACL grants access and must never be accepted for an export.
    if !present.as_bool() || acl.is_null() || !unsafe { Security::IsValidAcl(acl) }.as_bool() {
        return Err(unverified_security());
    }
    Ok(Dacl {
        pointer: acl,
        owner: descriptor,
    })
}

fn allowed_ace<'a>(acl: &Dacl<'a>, index: u32) -> io::Result<AllowedAce<'a>> {
    let mut pointer = std::ptr::null_mut();
    // GetAce bounds the index within the validated ACL and returns a borrowed ACE header.
    unsafe { Security::GetAce(acl.pointer, index, &mut pointer) }.map_err(io::Error::other)?;
    // GetAce succeeded, so the header resides in the live ACL allocation.
    let header = unsafe { pointer.cast::<Security::ACE_HEADER>().read_unaligned() };
    let sid_offset = mem::offset_of!(Security::ACCESS_ALLOWED_ACE, SidStart);
    let sid_header_bytes = mem::offset_of!(Security::SID, SubAuthority);
    if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
        || header.AceFlags != 0
        || usize::from(header.AceSize) < sid_offset + sid_header_bytes
    {
        return Err(unverified_security());
    }
    // The checked ACE size contains the mask and SID header; raw pointers retain the full allocation.
    let (mask, sid, subauthorities) = unsafe {
        let sid = pointer.byte_add(sid_offset);
        (
            pointer
                .byte_add(mem::offset_of!(Security::ACCESS_ALLOWED_ACE, Mask))
                .cast::<u32>()
                .read_unaligned(),
            Security::PSID(sid),
            sid.byte_add(mem::offset_of!(Security::SID, SubAuthorityCount))
                .cast::<u8>()
                .read(),
        )
    };
    if sid_offset + sid_header_bytes + usize::from(subauthorities) * mem::size_of::<u32>()
        > usize::from(header.AceSize)
    {
        return Err(unverified_security());
    }
    // The entire variable-length SID is now bounded by the validated ACE.
    if !unsafe { Security::IsValidSid(sid) }.as_bool() {
        return Err(unverified_security());
    }
    Ok(AllowedAce {
        mask,
        sid,
        _owner: acl.owner,
    })
}

fn same_ace(left: &AllowedAce<'_>, right: &AllowedAce<'_>) -> bool {
    // Both ACEs and their validated SID storage remain borrowed from live descriptors.
    left.mask == right.mask && unsafe { Security::EqualSid(left.sid, right.sid) }.is_ok()
}

fn unverified_security() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private export ACL could not be verified",
    )
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let disk_or_share = matches!(
        absolute.components().next(),
        Some(Component::Prefix(prefix)) if matches!(
            prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(..) | Prefix::VerbatimUNC(..)
        )
    );
    // Alternate streams reuse the base file's security descriptor even with CREATE_NEW.
    let alternate_stream = absolute.components().any(|component| {
        matches!(component, Component::Normal(name) if name.encode_wide().any(|character| character == u16::from(b':')))
    });
    if !disk_or_share || alternate_stream {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "export requires a regular file path",
        ));
    }
    let path: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    // Match std::fs's extended-path handling for long drive and UNC destinations.
    let mut wide = if path.starts_with(&[92, 92, 63, 92]) {
        path
    } else if path.starts_with(&[92, 92]) {
        r"\\?\UNC\"
            .encode_utf16()
            .chain(path.into_iter().skip(2))
            .collect()
    } else {
        r"\\?\".encode_utf16().chain(path).collect()
    };
    if wide.is_empty() || wide.len() >= 32_767 || wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid export path",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn security_descriptor(text: &str) -> io::Result<LocalAllocation> {
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor = Security::PSECURITY_DESCRIPTOR::default();
    // The terminated input and output slot are live; Windows allocates the returned descriptor.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(text.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(io::Error::other)?;
    Ok(LocalAllocation(descriptor.0))
}

fn current_user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // The current process pseudo-handle is valid and token points to initialized storage.
    unsafe { OpenProcessToken(GetCurrentProcess(), Security::TOKEN_QUERY, &mut token) }
        .map_err(io::Error::other)?;
    // OpenProcessToken transferred ownership of this handle to the caller.
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let mut buffer = [0_usize; 128];
    let mut written = 0;
    // This aligned fixed buffer exceeds TOKEN_USER plus the maximum Windows SID size.
    unsafe {
        Security::GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            Security::TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            u32::try_from(mem::size_of_val(&buffer)).unwrap(),
            &mut written,
        )
    }
    .map_err(io::Error::other)?;
    if (written as usize) < mem::size_of::<Security::TOKEN_USER>() {
        return Err(io::Error::other(
            "Windows returned an incomplete token user",
        ));
    }
    // A successful TokenUser query initializes TOKEN_USER and a SID within this live buffer.
    let sid = unsafe { (*buffer.as_ptr().cast::<Security::TOKEN_USER>()).User.Sid };
    let mut text = PWSTR::null();
    // The SID remains in buffer; Windows allocates the terminated output string.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.map_err(io::Error::other)?;
    let _allocation = LocalAllocation(text.0.cast());
    // The successful conversion supplies a valid terminated UTF-16 string until allocation drops.
    unsafe { text.to_string() }.map_err(io::Error::other)
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
