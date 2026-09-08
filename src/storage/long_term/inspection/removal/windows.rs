use cap_std::fs::{Dir, File, OpenOptions, OpenOptionsExt};
use std::{
    ffi::OsStr,
    io, mem,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle},
    },
};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, GENERIC_ALL, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, LocalFree,
        },
        Security::{
            self,
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
            },
        },
        Storage::FileSystem::*,
        System::{
            SystemServices::FILE_PERSISTENT_ACLS,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
    core::{PCWSTR, PWSTR},
};

struct Allocation(*mut std::ffi::c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

pub(super) fn handle(value: &impl AsRawHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}

pub(super) fn validate_directory(directory: &Dir, private: bool) -> io::Result<()> {
    reject_reparse(handle(directory))?;
    let mut filesystem = [0_u16; 32];
    let mut flags = 0;
    unsafe {
        GetVolumeInformationByHandleW(
            handle(directory),
            None,
            None,
            None,
            Some(&mut flags),
            Some(&mut filesystem),
        )
    }
    .map_err(|error| operation_error("query volume", error))?;
    let name = String::from_utf16_lossy(
        &filesystem[..filesystem
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(filesystem.len())],
    );
    if name != "NTFS" || flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "recording removal requires NTFS persistent ACLs and metadata flushing",
        ));
    }
    validate_security(handle(directory), private)
}

pub(super) fn create_private(parent: &Dir, name: &OsStr) -> io::Result<()> {
    let owner = user_sid()?;
    let sid = user_string(&owner)?;
    let descriptor_text: Vec<u16> =
        format!("O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
    let mut descriptor = Security::PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(descriptor_text.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(io::Error::other)?;
    let allocation = Allocation(descriptor.0);
    let attributes = Security::SECURITY_ATTRIBUTES {
        nLength: u32::try_from(mem::size_of::<Security::SECURITY_ATTRIBUTES>()).unwrap(),
        lpSecurityDescriptor: allocation.0,
        bInheritHandle: false.into(),
    };
    let filename = child_name(parent, name)?;
    unsafe { CreateDirectoryW(PCWSTR(filename.as_ptr()), Some(&attributes)) }
        .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))
}

fn child_name(parent: &Dir, name: &OsStr) -> io::Result<Vec<u16>> {
    let mut buffer = vec![0_u16; 4_096];
    let length =
        unsafe { GetFinalPathNameByHandleW(handle(parent), &mut buffer, FILE_NAME_NORMALIZED) }
            as usize;
    if length == 0 || length >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(length);
    let child: Vec<u16> = name.encode_wide().collect();
    if child.is_empty()
        || child.len() > 64
        || child
            .iter()
            .any(|character| matches!(*character, 0 | 47 | 58 | 92))
    {
        return Err(super::super::denied());
    }
    buffer.push(92);
    buffer.extend(child);
    buffer.push(0);
    Ok(buffer)
}

fn user_sid() -> io::Result<Vec<usize>> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), Security::TOKEN_QUERY, &mut token) }
        .map_err(|error| operation_error("open process token", error))?;
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let mut buffer = vec![0_usize; 128];
    let mut written = 0;
    unsafe {
        Security::GetTokenInformation(
            handle(&token),
            Security::TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            u32::try_from(buffer.len() * mem::size_of::<usize>()).unwrap(),
            &mut written,
        )
    }
    .map_err(|error| operation_error("query token user", error))?;
    if usize::try_from(written).unwrap() < mem::size_of::<Security::TOKEN_USER>() {
        return Err(super::super::denied());
    }
    Ok(buffer)
}

fn token_sid(buffer: &[usize]) -> Security::PSID {
    unsafe { (*buffer.as_ptr().cast::<Security::TOKEN_USER>()).User.Sid }
}

fn user_string(buffer: &[usize]) -> io::Result<String> {
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(token_sid(buffer), &mut text) }.map_err(io::Error::other)?;
    let _allocation = Allocation(text.0.cast());
    unsafe { text.to_string() }.map_err(io::Error::other)
}

fn validate_security(object: HANDLE, private: bool) -> io::Result<()> {
    let user = user_sid()?;
    let mut owner = Security::PSID::default();
    let mut acl = std::ptr::null_mut();
    let mut descriptor = Security::PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            object,
            SE_FILE_OBJECT,
            Security::OWNER_SECURITY_INFORMATION | Security::DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut acl),
            None,
            Some(&mut descriptor),
        )
    }
    .ok()
    .map_err(|error| operation_error("query security descriptor", error))?;
    let allocation = Allocation(descriptor.0);
    if acl.is_null() || owner.0.is_null() {
        return Err(super::super::denied());
    }
    unsafe { Security::EqualSid(owner, token_sid(&user)) }.map_err(|_| super::super::denied())?;
    let mut control = 0;
    let mut revision = 0;
    unsafe { Security::GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) }
        .map_err(|error| operation_error("query security control", error))?;
    if private && control & Security::SE_DACL_PROTECTED.0 == 0 {
        return Err(super::super::denied());
    }
    let bytes = acl_bytes(&allocation, acl)?;
    let count = u16::from_le_bytes([bytes[4], bytes[5]]);
    if count > 128 {
        return Err(super::super::denied());
    }
    for index in 0..count {
        validate_ace(acl, bytes, u32::from(index), token_sid(&user), private)?;
    }
    Ok(())
}

fn acl_bytes(allocation: &Allocation, acl: *const Security::ACL) -> io::Result<&[u8]> {
    let length = unsafe {
        Security::GetSecurityDescriptorLength(Security::PSECURITY_DESCRIPTOR(allocation.0))
    } as usize;
    let offset = acl
        .addr()
        .checked_sub(allocation.0.addr())
        .ok_or_else(super::super::denied)?;
    if length > 256 * 1024 || offset.checked_add(8).is_none_or(|end| end > length) {
        return Err(super::super::denied());
    }
    let header = unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), 8) };
    let size = usize::from(u16::from_le_bytes([header[2], header[3]]));
    if size < 8 || offset.checked_add(size).is_none_or(|end| end > length) {
        return Err(super::super::denied());
    }
    if !unsafe { Security::IsValidAcl(acl) }.as_bool() {
        return Err(super::super::denied());
    }
    Ok(unsafe { std::slice::from_raw_parts(acl.cast::<u8>(), size) })
}

fn validate_ace(
    acl: *const Security::ACL,
    bytes: &[u8],
    index: u32,
    user: Security::PSID,
    private: bool,
) -> io::Result<()> {
    let mut pointer = std::ptr::null_mut();
    unsafe { Security::GetAce(acl, index, &mut pointer) }.map_err(io::Error::other)?;
    let offset = pointer
        .addr()
        .checked_sub(acl.addr())
        .ok_or_else(super::super::denied)?;
    let header = bytes
        .get(offset..)
        .filter(|value| value.len() >= 4)
        .ok_or_else(super::super::denied)?;
    let size = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let (mask, sid_bytes) =
        super::ace::allowed(header.get(..size).ok_or_else(super::super::denied)?)?;
    let sid = Security::PSID(sid_bytes.as_ptr().cast_mut().cast());
    if !unsafe { Security::IsValidSid(sid) }.as_bool() {
        return Err(super::super::denied());
    }
    let trusted = unsafe {
        Security::EqualSid(sid, user).is_ok()
            || Security::IsWellKnownSid(sid, Security::WinLocalSystemSid).as_bool()
            || Security::IsWellKnownSid(sid, Security::WinBuiltinAdministratorsSid).as_bool()
    };
    let mutation = FILE_WRITE_DATA.0
        | FILE_APPEND_DATA.0
        | FILE_WRITE_EA.0
        | FILE_WRITE_ATTRIBUTES.0
        | FILE_DELETE_CHILD.0
        | DELETE.0
        | WRITE_DAC.0
        | WRITE_OWNER.0
        | GENERIC_WRITE.0
        | GENERIC_ALL.0;
    if !trusted && (private || mask & mutation != 0) {
        return Err(super::super::denied());
    }
    Ok(())
}

pub(super) fn exclusive_file(directory: &Dir, name: &OsStr) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .access_mode(GENERIC_READ.0 | DELETE.0)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    let file = directory.open_with(name, &options)?;
    reject_reparse(handle(&file))?;
    validate_security(handle(&file), false)?;
    super::super::eligible(&file.metadata()?)?;
    Ok(file)
}

fn reject_reparse(object: HANDLE) -> io::Result<()> {
    let mut attributes = FILE_ATTRIBUTE_TAG_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            object,
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut attributes).cast(),
            u32::try_from(mem::size_of_val(&attributes)).unwrap(),
        )
    }
    .map_err(io::Error::other)?;
    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(super::super::denied());
    }
    Ok(())
}

pub(super) fn rename(file: &File, directory: &Dir) -> io::Result<()> {
    let name = child_name(directory, OsStr::new(super::STAGED_FILE))?;
    let bytes = mem::offset_of!(FILE_RENAME_INFO, FileName) + name.len() * 2;
    let mut buffer = vec![0_usize; bytes.div_ceil(mem::size_of::<usize>())];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*information).Anonymous.ReplaceIfExists = false;
        (*information).RootDirectory = HANDLE::default();
        (*information).FileNameLength = u32::try_from((name.len() - 1) * 2).unwrap();
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast(),
            name.len(),
        );
        SetFileInformationByHandle(
            handle(file),
            FileRenameInfo,
            information.cast(),
            u32::try_from(bytes).unwrap(),
        )
    }
    .map_err(|error| operation_error("rename selected handle", error))
}

pub(super) fn remove(file: File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(
            FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0,
        ),
    };
    unsafe {
        SetFileInformationByHandle(
            handle(&file),
            FileDispositionInfoEx,
            std::ptr::from_ref(&disposition).cast(),
            u32::try_from(mem::size_of_val(&disposition)).unwrap(),
        )
    }
    .map_err(io::Error::other)?;
    let raw = file.into_std().into_raw_handle();
    unsafe { CloseHandle(HANDLE(raw)) }.map_err(io::Error::other)
}

pub(super) fn sync(directory: &Dir) -> io::Result<()> {
    use cap_fs_ext::OpenOptionsMaybeDirExt;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .maybe_dir(true)
        .access_mode(GENERIC_READ.0 | GENERIC_WRITE.0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0);
    directory
        .open_with(".", &options)
        .map_err(|error| {
            io::Error::new(error.kind(), format!("open directory for flush: {error}"))
        })?
        .sync_all()
        .map_err(|error| io::Error::new(error.kind(), format!("flush directory: {error}")))
}

fn operation_error(operation: &str, error: windows::core::Error) -> io::Error {
    io::Error::other(format!("{operation}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntfs_primitives_qualify_rename_flush_and_remove_the_selected_file() {
        let root =
            std::env::temp_dir().join(format!("keeppeek-ntfs-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        let directory = Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        let result = (|| -> io::Result<()> {
            validate_directory(&directory, false)?;
            sync(&directory)?;
            create_private(&directory, OsStr::new("staging"))?;
            let staging = directory.open_dir("staging")?;
            validate_directory(&staging, true)?;
            directory.write("selected.mp4", [42; 8])?;
            let selected = exclusive_file(&directory, OsStr::new("selected.mp4"))?;
            rename(&selected, &staging)?;
            sync(&directory)?;
            sync(&staging)?;
            assert_eq!(staging.read(super::super::STAGED_FILE)?, [42; 8]);
            remove(selected)?;
            sync(&staging)?;
            assert!(!staging.try_exists(super::super::STAGED_FILE)?);
            Ok(())
        })();
        drop(directory);
        std::fs::remove_dir_all(root).unwrap();
        result.unwrap();
    }
}
