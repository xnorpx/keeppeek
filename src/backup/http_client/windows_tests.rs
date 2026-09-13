use super::{LocalAllocation, current_user_sid, security_descriptor, verify_descriptor, wide_path};
use crate::backup::http_client::{
    create_private_file,
    tests::{SYNTHETIC_ARCHIVE, TestDirectory, export_from_fixture},
};
use std::{fs::File, mem, os::windows::io::AsRawHandle, path::Path};
use windows::{
    Win32::{
        Foundation::HANDLE,
        Security::{
            self,
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
                ConvertStringSidToSidW, GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
            },
        },
        Storage::FileSystem::CreateDirectoryW,
    },
    core::{PCWSTR, PWSTR},
};

fn readable_parent(path: &Path) {
    let owner = current_user_sid().unwrap();
    let descriptor = security_descriptor(&format!(
        "O:{owner}D:P(A;OICI;FA;;;{owner})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;BU)"
    ))
    .unwrap();
    let attributes = Security::SECURITY_ATTRIBUTES {
        nLength: u32::try_from(mem::size_of::<Security::SECURITY_ATTRIBUTES>()).unwrap(),
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let filename = wide_path(path).unwrap();
    // The converted descriptor and terminated filename remain live for the call.
    unsafe { CreateDirectoryW(PCWSTR(filename.as_ptr()), Some(&attributes)) }.unwrap();
    let inherited = path.join("inherited.txt");
    std::fs::write(&inherited, b"public fixture").unwrap();
    assert!(dacl(&File::open(&inherited).unwrap()).contains(";;;BU)"));
    std::fs::remove_file(inherited).unwrap();
}

fn dacl(file: &File) -> String {
    let mut descriptor = Security::PSECURITY_DESCRIPTOR::default();
    // The file owns a live handle; Windows allocates a complete descriptor for the output slot.
    unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            Security::DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    }
    .ok()
    .unwrap();
    let _descriptor = LocalAllocation(descriptor.0);
    let mut text = PWSTR::null();
    // The descriptor remains allocated while Windows creates the terminated SDDL output.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            Security::DACL_SECURITY_INFORMATION,
            &mut text,
            None,
        )
    }
    .unwrap();
    let _text = LocalAllocation(text.0.cast());
    // The conversion owns a terminated UTF-16 string until _text drops.
    unsafe { text.to_string() }.unwrap()
}

fn assert_private(file: &File) {
    let descriptor = dacl(file);
    assert!(
        descriptor.starts_with("D:P"),
        "DACL must block inheritance: {descriptor}"
    );
    let mut trustees: Vec<_> = descriptor
        .split('(')
        .skip(1)
        .map(|ace| {
            assert!(ace.starts_with("A;;FA;;;"), "unexpected access rule: {ace}");
            canonical_sid(ace.trim_end_matches(')').rsplit(';').next().unwrap())
        })
        .collect();
    trustees.sort();
    let mut trusted = vec![
        current_user_sid().unwrap(),
        "S-1-5-18".to_owned(),
        "S-1-5-32-544".to_owned(),
    ];
    trusted.sort();
    assert_eq!(trustees, trusted);
}

fn canonical_sid(trustee: &str) -> String {
    let input: Vec<u16> = trustee.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sid = Security::PSID::default();
    // SDDL aliases such as SY must compare equal to the current user's raw SID.
    unsafe { ConvertStringSidToSidW(PCWSTR(input.as_ptr()), &mut sid) }.unwrap();
    let _sid = LocalAllocation(sid.0);
    let mut text = PWSTR::null();
    // Windows owns the parsed SID until _sid drops and allocates the output string.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.unwrap();
    let _text = LocalAllocation(text.0.cast());
    // The successful conversion returns terminated UTF-16 text that remains live here.
    unsafe { text.to_string() }.unwrap()
}

#[test]
fn inherited_reader_access_is_blocked_before_any_export_bytes_are_written() {
    let directory = TestDirectory::new();
    let parent = directory.0.join("readable");
    readable_parent(&parent);
    let destination = parent.join("configuration.zip");
    let file = create_private_file(&destination).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 0);
    assert_private(&file);
}

#[test]
fn complete_export_remains_private_in_a_readable_destination_directory() {
    let directory = TestDirectory::new();
    let parent = directory.0.join("readable");
    readable_parent(&parent);
    let destination = parent.join("configuration.zip");
    assert_eq!(
        export_from_fixture(&destination, SYNTHETIC_ARCHIVE.len()).unwrap(),
        SYNTHETIC_ARCHIVE.len() as u64
    );
    assert_eq!(std::fs::read(&destination).unwrap(), SYNTHETIC_ARCHIVE);
    assert_private(&File::open(destination).unwrap());
}

#[test]
fn normal_private_export_is_readable_by_its_owner() {
    let directory = TestDirectory::new();
    let destination = directory.0.join("configuration.zip");
    assert_eq!(
        export_from_fixture(&destination, SYNTHETIC_ARCHIVE.len()).unwrap(),
        SYNTHETIC_ARCHIVE.len() as u64
    );
    assert_eq!(std::fs::read(&destination).unwrap(), SYNTHETIC_ARCHIVE);
    assert_private(&File::open(destination).unwrap());
}

#[test]
fn private_export_preserves_windows_extended_length_paths() {
    let directory = TestDirectory::new();
    let parent = directory.0.join("a".repeat(100)).join("b".repeat(100));
    std::fs::create_dir_all(&parent).unwrap();
    let destination = parent.join("configuration.zip");
    let file = create_private_file(&destination).unwrap();
    assert_private(&file);
}

#[test]
fn alternate_stream_cannot_reuse_a_public_files_security_descriptor() {
    let directory = TestDirectory::new();
    let public = directory.0.join("public.txt");
    std::fs::write(&public, b"public data").unwrap();
    let destination = directory.0.join("public.txt:configuration.zip");
    assert!(create_private_file(&destination).is_err());
    assert!(!destination.exists());
    assert_eq!(std::fs::read(public).unwrap(), b"public data");
}

#[test]
fn failed_export_leaves_no_private_bytes_in_a_readable_directory() {
    let directory = TestDirectory::new();
    let parent = directory.0.join("readable");
    readable_parent(&parent);
    let destination = parent.join("configuration.zip");
    assert!(export_from_fixture(&destination, SYNTHETIC_ARCHIVE.len() + 1).is_err());
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
}

#[test]
fn readback_rejects_unprotected_missing_null_and_unexpected_access_rules() {
    let owner = current_user_sid().unwrap();
    let grants = format!("(A;;FA;;;{owner})(A;;FA;;;SY)(A;;FA;;;BA)");
    let expected = security_descriptor(&format!("O:{owner}D:P{grants}")).unwrap();
    for text in [
        format!("O:{owner}D:{grants}"),
        format!("O:{owner}"),
        format!("O:{owner}D:NO_ACCESS_CONTROL"),
        format!("O:{owner}D:P(A;;FA;;;WD){grants}"),
        format!("O:{owner}D:P(A;;FA;;;{owner})(A;;FA;;;SY)"),
        format!("O:{owner}D:P(A;ID;FA;;;{owner})(A;;FA;;;SY)(A;;FA;;;BA)"),
        format!("O:BAD:P{grants}"),
        format!("O:{owner}D:P(A;;FA;;;{owner})(D;;FR;;;SY)(A;;FA;;;BA)"),
        format!("O:{owner}D:P(A;;FR;;;{owner})(A;;FA;;;SY)(A;;FA;;;BA)"),
    ] {
        let actual = security_descriptor(&text).unwrap();
        assert!(
            verify_descriptor(&actual, &expected).is_err(),
            "accepted unsafe descriptor: {text}"
        );
    }
}

#[test]
fn readback_accepts_the_same_private_trustees_in_a_different_order() {
    let owner = current_user_sid().unwrap();
    let expected = security_descriptor(&format!(
        "O:{owner}D:P(A;;FA;;;{owner})(A;;FA;;;SY)(A;;FA;;;BA)"
    ))
    .unwrap();
    let actual = security_descriptor(&format!(
        "O:{owner}D:P(A;;FA;;;BA)(A;;FA;;;{owner})(A;;FA;;;SY)"
    ))
    .unwrap();
    verify_descriptor(&actual, &expected).unwrap();
}

#[test]
fn readback_accepts_system_owner_with_the_explicit_duplicate_system_grant() {
    let expected = security_descriptor("O:SYD:P(A;;FA;;;SY)(A;;FA;;;SY)(A;;FA;;;BA)").unwrap();
    let actual =
        security_descriptor("O:S-1-5-18D:P(A;;FA;;;BA)(A;;FA;;;S-1-5-18)(A;;FA;;;SY)").unwrap();
    verify_descriptor(&actual, &expected).unwrap();
}
