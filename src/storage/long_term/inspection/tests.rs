use super::{Archive, Identity};
use cap_fs_ext::MetadataExt;
use std::{io::Write, path::PathBuf};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("keeppeek-file-pin-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("recording.mp4"), [42; 64]).unwrap();
        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn observation_pins_the_original_file_without_write_access_after_path_replacement() {
    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    let recording = fixture.root.join("recording.mp4");
    let observation = archive.inspect(&recording, 64).unwrap();
    std::fs::rename(&recording, fixture.root.join("original.mp4")).unwrap();
    std::fs::write(&recording, [24; 65]).unwrap();
    let metadata = observation.file.metadata().unwrap();
    assert_eq!(metadata.len(), 64);
    assert_eq!(metadata.dev(), observation.identity().device);
    assert_eq!(metadata.ino(), observation.identity().file);
    assert!(
        observation
            .file
            .try_clone()
            .unwrap()
            .write_all(&[0; 64])
            .is_err()
    );
    assert!(archive.revalidate(&observation).is_err());
    assert_eq!(
        std::fs::read(fixture.root.join("original.mp4")).unwrap(),
        [42; 64]
    );
    assert_eq!(std::fs::read(recording).unwrap(), [24; 65]);
}

#[test]
fn revalidation_respects_the_parent_deadline_before_reading_pinned_metadata() {
    use std::time::{Duration, Instant};

    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    let recording = fixture.root.join("recording.mp4");
    let observation = archive.inspect(&recording, 64).unwrap();
    archive
        .revalidate_until(&observation, Instant::now() + Duration::from_secs(2))
        .unwrap();
    let deadline = Instant::now();
    std::fs::remove_file(recording).unwrap();
    let error = archive
        .revalidate_until(&observation, deadline)
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(observation.file.metadata().unwrap().len(), 64);
}

#[test]
fn catalog_identifiers_are_bounded_and_redacted() {
    for invalid in [
        "",
        ":",
        "1:",
        ":2",
        "1:2:3",
        "1:-2",
        "+1:2",
        "1:18446744073709551616",
    ] {
        assert!(Identity::parse(invalid).is_none());
    }
    let boundary = Identity::parse("18446744073709551615:18446744073709551615").unwrap();
    assert_eq!(boundary.device, u64::MAX);
    assert_eq!(boundary.file, u64::MAX);
    assert_eq!(format!("{boundary:?}"), "Identity([REDACTED])");
    assert!(Identity::parse(&format!("{}:1", "0".repeat(42))).is_none());
}

#[cfg(unix)]
#[test]
fn leaf_swapped_to_fifo_after_metadata_check_does_not_wait_for_a_writer() {
    use std::{
        ffi::OsStr,
        sync::mpsc,
        time::{Duration, Instant},
    };

    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    let recording = fixture.root.join("recording.mp4");
    super::eligible(&archive.directory.symlink_metadata("recording.mp4").unwrap()).unwrap();
    std::fs::rename(&recording, fixture.root.join("original.mp4")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&recording)
            .status()
            .unwrap()
            .success()
    );
    let directory = archive.directory.try_clone().unwrap();
    let (reply, response) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = super::open_leaf(
            &directory,
            OsStr::new("recording.mp4"),
            Instant::now() + Duration::from_secs(2),
        );
        assert!(reply.send(result.map(|_| ())).is_ok());
    });
    let error = response
        .recv_timeout(Duration::from_secs(2))
        .expect("FIFO open must not wait for a writer")
        .unwrap_err();
    worker.join().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        std::fs::read(fixture.root.join("original.mp4")).unwrap(),
        [42; 64]
    );
}

#[cfg(unix)]
#[test]
fn leaf_swapped_to_symlink_after_metadata_check_never_follows_its_target() {
    use std::{
        ffi::OsStr,
        time::{Duration, Instant},
    };

    let fixture = Fixture::new();
    let outside = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    super::eligible(&archive.directory.symlink_metadata("recording.mp4").unwrap()).unwrap();
    std::fs::rename(
        fixture.root.join("recording.mp4"),
        fixture.root.join("original.mp4"),
    )
    .unwrap();
    std::fs::write(outside.root.join("recording.mp4"), [24; 64]).unwrap();
    std::os::unix::fs::symlink(
        outside.root.join("recording.mp4"),
        fixture.root.join("recording.mp4"),
    )
    .unwrap();
    assert!(
        super::open_leaf(
            &archive.directory,
            OsStr::new("recording.mp4"),
            Instant::now() + Duration::from_secs(2)
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.root.join("original.mp4")).unwrap(),
        [42; 64]
    );
    assert_eq!(
        std::fs::read(outside.root.join("recording.mp4")).unwrap(),
        [24; 64]
    );
}

#[test]
fn hard_link_added_after_metadata_check_is_rejected_from_handle_metadata() {
    use std::{
        ffi::OsStr,
        time::{Duration, Instant},
    };

    let fixture = Fixture::new();
    let archive = Archive::open(&fixture.root).unwrap();
    super::eligible(&archive.directory.symlink_metadata("recording.mp4").unwrap()).unwrap();
    std::fs::hard_link(
        fixture.root.join("recording.mp4"),
        fixture.root.join("alias.mp4"),
    )
    .unwrap();
    let error = super::open_leaf(
        &archive.directory,
        OsStr::new("recording.mp4"),
        Instant::now() + Duration::from_secs(2),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    assert_eq!(
        std::fs::read(fixture.root.join("alias.mp4")).unwrap(),
        [42; 64]
    );
}
