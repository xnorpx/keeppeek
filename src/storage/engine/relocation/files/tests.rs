use super::*;

#[test]
fn parent_aliases_keep_the_single_directory_entry() {
    let root = std::env::temp_dir().join(format!("keeppeek-alias-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(root.join("sub")).unwrap();
    let source = root.join("file.mp4");
    fs::write(&source, b"recording").unwrap();
    let alias = root.join("sub").join("..").join("file.mp4");
    assert!(same_entry(&source, &alias).unwrap());
    let other = root.join("sub/file.mp4");
    fs::hard_link(&source, &other).unwrap();
    assert!(!same_entry(&source, &other).unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsupported_rename_publishes_by_link_without_replacing_existing_bytes() {
    let root = std::env::temp_dir().join(format!("keeppeek-fallback-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let directory =
        cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
    let publish = || {
        publish_no_replace(&directory, "stage".as_ref(), "object.mp4".as_ref(), || {
            Err(io::ErrorKind::Unsupported.into())
        })
    };
    fs::write(root.join("stage"), b"first").unwrap();
    publish().unwrap();
    assert!(!root.join("stage").exists());
    fs::write(root.join("stage"), b"second").unwrap();
    assert!(publish().is_err());
    assert_eq!(fs::read(root.join("object.mp4")).unwrap(), b"first");
    drop(directory);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cross_filesystem_copy_publishes_exact_bytes_and_preserves_a_conflicting_destination() {
    let root = std::env::temp_dir().join(format!("keeppeek-relocate-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let source = root.join("source.mp4");
    let destination = root.join("destination.mp4");
    fs::write(&source, b"first recording").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    }
    copy_and_publish(&source, &destination, &root, uuid::Uuid::new_v4()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(verify_existing(&source, &destination).is_err());
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(fs::read(&source).unwrap(), b"first recording");
    assert_eq!(fs::read(&destination).unwrap(), b"first recording");
    copy_and_publish(&source, &destination, &root, uuid::Uuid::new_v4()).unwrap();
    fs::write(&source, b"later recording").unwrap();
    assert!(copy_and_publish(&source, &destination, &root, uuid::Uuid::new_v4()).is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"first recording");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    fs::remove_dir_all(root).unwrap();
}
