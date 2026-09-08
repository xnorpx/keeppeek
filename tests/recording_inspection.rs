use keeppeek::storage::long_term::inspection::Archive;
use std::io::ErrorKind;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    recording: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-file-inspection-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir_all(root.join("camera/day")).unwrap();
        let recording = root.join("camera/day/recording.mp4");
        std::fs::write(&recording, [42; 64]).unwrap();
        Self { root, recording }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn same_size_replacement_invalidates_the_observation_without_removing_either_file() {
    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    let observation = archive.inspect(&fixture.recording, 64).unwrap();
    assert_eq!(observation.file_bytes(), 64);
    archive.revalidate(&observation).unwrap();

    let original = fixture.root.join("camera/day/original.mp4");
    std::fs::rename(&fixture.recording, &original).unwrap();
    std::fs::write(&fixture.recording, [24; 64]).unwrap();
    assert_eq!(
        archive.revalidate(&observation).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(std::fs::read(original).unwrap(), [42; 64]);
    assert_eq!(std::fs::read(&fixture.recording).unwrap(), [24; 64]);
}

#[test]
fn changed_permissions_invalidate_the_observation() {
    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    let observation = archive.inspect(&fixture.recording, 64).unwrap();
    let permissions = std::fs::metadata(&fixture.recording).unwrap().permissions();
    let mut changed = permissions.clone();
    changed.set_readonly(true);
    std::fs::set_permissions(&fixture.recording, changed).unwrap();
    let result = archive.revalidate(&observation);
    std::fs::set_permissions(&fixture.recording, permissions).unwrap();
    assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidData);
}

#[test]
fn stale_catalog_size_and_missing_or_nonregular_files_are_rejected() {
    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    assert_eq!(
        archive.inspect(&fixture.recording, 65).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert_eq!(
        archive
            .inspect(fixture.root.join("missing.mp4"), 64)
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    let directory = fixture.root.join("directory.mp4");
    std::fs::create_dir(directory.clone()).unwrap();
    assert_eq!(
        archive.inspect(directory, 64).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    let observation = archive.inspect(&fixture.recording, 64).unwrap();
    std::fs::write(&fixture.recording, [42; 128]).unwrap();
    assert_eq!(
        archive.revalidate(&observation).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn traversal_hidden_names_and_unbounded_paths_are_rejected_before_filesystem_lookup() {
    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    for relative in [
        "../recording.mp4".to_owned(),
        "camera/../../recording.mp4".to_owned(),
        ".exports/recording.mp4".to_owned(),
        "camera/.hidden/recording.mp4".to_owned(),
        "camera/alternate:stream.mp4".to_owned(),
        "camera/recording\0.mp4".to_owned(),
        format!("{}recording.mp4", "nested/".repeat(16)),
        format!("{}.mp4", "a".repeat(4_096)),
    ] {
        assert_eq!(
            archive
                .inspect(fixture.root.join(relative), 64)
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
    }
    let other = Fixture::new();
    assert_eq!(
        archive.inspect(&other.recording, 64).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(std::fs::read(&fixture.recording).unwrap(), [42; 64]);
    assert_eq!(std::fs::read(&other.recording).unwrap(), [42; 64]);
}

#[test]
fn path_separator_rules_follow_the_host_filesystem() {
    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    let separated = fixture.root.join("camera\\day\\recording.mp4");
    let inspection = archive.inspect(separated, 64);
    #[cfg(unix)]
    assert_eq!(inspection.unwrap_err().kind(), ErrorKind::PermissionDenied);
    #[cfg(windows)]
    archive.revalidate(&inspection.unwrap()).unwrap();
}

#[test]
fn hard_linked_media_is_not_eligible_for_an_observation() {
    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    let observation = archive.inspect(&fixture.recording, 64).unwrap();
    let linked = fixture.root.join("alias.mp4");
    std::fs::hard_link(&fixture.recording, &linked).unwrap();
    assert_eq!(
        archive.inspect(&linked, 64).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(
        archive.revalidate(&observation).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert_eq!(std::fs::read(linked).unwrap(), [42; 64]);
}

#[test]
fn observations_are_instance_bound_and_debug_does_not_disclose_paths() {
    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    let observation = archive.inspect(&fixture.recording, 64).unwrap();
    let other_instance = Archive::open(&fixture.root).unwrap();
    assert_eq!(
        other_instance.revalidate(&observation).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    for debug in [format!("{archive:?}"), format!("{observation:?}")] {
        assert!(!debug.contains("recording.mp4"));
        assert!(!debug.contains(fixture.root.to_str().unwrap()));
    }
}

#[cfg(unix)]
#[test]
fn replacing_an_ancestor_with_a_symlink_cannot_redirect_inspection() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    let observation = archive.inspect(&fixture.recording, 64).unwrap();
    std::fs::rename(fixture.root.join("camera"), fixture.root.join("moved")).unwrap();
    std::os::unix::fs::symlink(outside.root.join("camera"), fixture.root.join("camera")).unwrap();
    assert!(archive.revalidate(&observation).is_err());
    assert!(archive.inspect(&fixture.recording, 64).is_err());
    assert_eq!(
        std::fs::read(fixture.root.join("moved/day/recording.mp4")).unwrap(),
        [42; 64]
    );
    assert_eq!(std::fs::read(&outside.recording).unwrap(), [42; 64]);
}

#[cfg(unix)]
#[test]
fn leaf_symlinks_and_in_root_directory_symlinks_are_rejected() {
    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    std::os::unix::fs::symlink(&fixture.recording, fixture.root.join("alias.mp4")).unwrap();
    std::os::unix::fs::symlink("camera", fixture.root.join("alias")).unwrap();
    assert_eq!(
        archive
            .inspect(fixture.root.join("alias.mp4"), 64)
            .unwrap_err()
            .kind(),
        ErrorKind::PermissionDenied
    );
    assert!(
        archive
            .inspect(fixture.root.join("alias/day/recording.mp4"), 64)
            .is_err()
    );
    assert_eq!(std::fs::read(&fixture.recording).unwrap(), [42; 64]);
}
