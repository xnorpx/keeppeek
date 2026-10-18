use super::*;
use std::{cell::Cell, io::Cursor, time::Duration};

struct Fixture {
    path: std::path::PathBuf,
    target: FilesystemArchiveTarget,
}

impl Fixture {
    fn new() -> anyhow::Result<Self> {
        use crate::storage::long_term::inspection::removal::create_private_directory;
        let base = std::env::temp_dir();
        #[cfg(unix)]
        let base = std::fs::canonicalize(base)?;
        let parent = cap_std::fs::Dir::open_ambient_dir(&base, cap_std::ambient_authority())?;
        let name = format!("keeppeek-archive-target-{}", uuid::Uuid::new_v4());
        drop(create_private_directory(
            &parent,
            std::ffi::OsStr::new(&name),
        )?);
        let path = base.join(name);
        let identity = Root::open(&path)?.identity().clone();
        let target = FilesystemArchiveTarget::open(&path, &identity)?;
        Ok(Self { path, target })
    }

    fn publish(&self, object: ObjectId, bytes: &[u8]) -> anyhow::Result<Version> {
        self.target.publish(
            object,
            &mut Cursor::new(bytes),
            bytes.len() as u64,
            None,
            &mut |_| Ok(()),
            &work(&|| false),
        )
    }

    fn close(self) -> anyhow::Result<()> {
        drop(self.target);
        std::fs::remove_dir_all(self.path)?;
        Ok(())
    }
}

fn work(cancelled: &dyn Fn() -> bool) -> Work<'_> {
    Work {
        deadline: std::time::Instant::now() + Duration::from_secs(30),
        cancelled,
    }
}

#[test]
fn filesystem_archive_publishes_and_reads_exact_journaled_version() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let object = ObjectId::random();
    let payload = vec![37_u8; 65_543];
    let mut checkpoints = Vec::new();
    let version = fixture.target.publish(
        object,
        &mut Cursor::new(&payload),
        payload.len() as u64,
        None,
        &mut |version| {
            checkpoints.push(version.clone());
            Ok(())
        },
        &work(&|| false),
    )?;
    assert!(checkpoints.len() >= 2);
    assert_eq!(checkpoints[0].bytes, 0);
    assert_eq!(checkpoints.last(), Some(&version));
    assert_eq!(checkpoints[0].token, version.token);
    assert!(!fixture.path.join(format!("{object}.tmp")).exists());
    fixture.target.inspect(&version, &work(&|| false))?;
    let mut observed = Vec::new();
    fixture
        .target
        .read(&version, &mut observed, &work(&|| false))?;
    assert_eq!(observed, payload);
    fixture.close()
}

#[test]
fn filesystem_archive_recovers_checkpointed_stage_and_completed_publication() -> anyhow::Result<()>
{
    let fixture = Fixture::new()?;
    let object = ObjectId::random();
    let payload = b"durable evidence";
    let mut staged = None;
    let failed = fixture.target.publish(
        object,
        &mut Cursor::new(payload),
        payload.len() as u64,
        None,
        &mut |version| {
            staged = Some(version.clone());
            anyhow::ensure!(version.bytes == 0, "injected crash before rename");
            Ok(())
        },
        &work(&|| false),
    );
    assert!(failed.is_err());
    assert!(!fixture.path.join(format!("{object}.mp4")).exists());
    let version = fixture.target.publish(
        object,
        &mut Cursor::new(payload),
        payload.len() as u64,
        staged.as_ref(),
        &mut |_| Ok(()),
        &work(&|| false),
    )?;
    let reopened = FilesystemArchiveTarget::open(&fixture.path, fixture.target.root.identity())?;
    let retried = reopened.publish(
        object,
        &mut Cursor::new([]),
        payload.len() as u64,
        Some(&version),
        &mut |_| anyhow::bail!("published retry must not change the journal"),
        &work(&|| false),
    )?;
    assert_eq!(retried, version);
    assert_eq!(
        std::fs::read(fixture.path.join(format!("{object}.mp4")))?,
        payload
    );
    drop(reopened);
    fixture.close()
}

#[test]
fn filesystem_archive_never_adopts_or_overwrites_unknown_objects() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    for extension in ["tmp", "mp4"] {
        let object = ObjectId::random();
        let path = fixture.path.join(format!("{object}.{extension}"));
        std::fs::write(&path, b"unrelated")?;
        assert!(fixture.publish(object, b"new bytes").is_err());
        assert_eq!(std::fs::read(path)?, b"unrelated");
    }
    fixture.close()
}

#[test]
fn filesystem_archive_checks_cancellation_before_writes_and_can_retry() -> anyhow::Result<()> {
    struct CancellingReader<'a> {
        bytes: Cursor<Vec<u8>>,
        cancelled: &'a Cell<bool>,
    }
    impl Read for CancellingReader<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let read = self.bytes.read(bytes)?;
            self.cancelled.set(true);
            Ok(read)
        }
    }
    let fixture = Fixture::new()?;
    let object = ObjectId::random();
    let cancelled = Cell::new(false);
    let payload = vec![3_u8; 65_537];
    let mut source = CancellingReader {
        bytes: Cursor::new(payload.clone()),
        cancelled: &cancelled,
    };
    let mut staged = None;
    assert!(
        fixture
            .target
            .publish(
                object,
                &mut source,
                payload.len() as u64,
                None,
                &mut |version| {
                    staged = Some(version.clone());
                    Ok(())
                },
                &work(&|| cancelled.get()),
            )
            .is_err()
    );
    assert_eq!(
        std::fs::metadata(fixture.path.join(format!("{object}.tmp")))?.len(),
        0
    );
    fixture.target.publish(
        object,
        &mut Cursor::new(&payload),
        payload.len() as u64,
        staged.as_ref(),
        &mut |_| Ok(()),
        &work(&|| false),
    )?;
    fixture.close()
}

#[test]
fn filesystem_archive_rejects_expired_work_before_creating_objects() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let object = ObjectId::random();
    let work = Work {
        deadline: std::time::Instant::now(),
        cancelled: &|| false,
    };
    assert!(
        fixture
            .target
            .publish(
                object,
                &mut Cursor::new(b"data"),
                4,
                None,
                &mut |_| Ok(()),
                &work,
            )
            .is_err()
    );
    assert_eq!(std::fs::read_dir(&fixture.path)?.count(), 0);
    fixture.close()
}

#[test]
fn filesystem_archive_rejects_short_and_growing_sources() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    for bytes in [3, 5] {
        let object = ObjectId::random();
        assert!(
            fixture
                .target
                .publish(
                    object,
                    &mut Cursor::new(b"data"),
                    bytes,
                    None,
                    &mut |_| Ok(()),
                    &work(&|| false),
                )
                .is_err()
        );
        assert!(!fixture.path.join(format!("{object}.mp4")).exists());
    }
    fixture.close()
}

#[test]
fn filesystem_archive_rejects_replaced_objects_and_wrong_root_binding() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let version = fixture.publish(ObjectId::random(), b"original")?;
    let key = format!("{}.mp4", version.object);
    std::fs::rename(fixture.path.join(&key), fixture.path.join("retained"))?;
    std::fs::write(fixture.path.join(&key), b"replaced")?;
    assert!(fixture.target.inspect(&version, &work(&|| false)).is_err());
    assert!(
        fixture
            .target
            .read(&version, &mut Vec::new(), &work(&|| false))
            .is_err()
    );
    let mut wrong_root = fixture.target.root.identity().clone();
    wrong_root.directory.push('x');
    assert!(FilesystemArchiveTarget::open(&fixture.path, &wrong_root).is_err());
    assert_eq!(std::fs::read(fixture.path.join(key))?, b"replaced");
    fixture.close()
}

#[test]
fn filesystem_archive_deletion_requires_exact_digest_and_durable_receipt() -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new()?;
    let version = fixture.publish(ObjectId::random(), b"retained")?;
    let operation = ObjectId::random();
    assert!(
        fixture
            .target
            .delete(&version, [0; 32], operation, &work(&|| false))
            .is_err()
    );
    fixture.target.inspect(&version, &work(&|| false))?;
    let digest = Sha256::digest(b"retained").into();
    fixture
        .target
        .delete(&version, digest, operation, &work(&|| false))?;
    fixture
        .target
        .delete(&version, digest, operation, &work(&|| false))?;
    assert!(
        !fixture
            .path
            .join(format!("{}.mp4", version.object))
            .exists()
    );
    fixture
        .target
        .acknowledge_delete(&version, digest, operation, &work(&|| false))?;
    fixture
        .target
        .acknowledge_delete(&version, digest, operation, &work(&|| false))?;
    fixture.close()
}

#[cfg(unix)]
#[test]
fn filesystem_archive_rejects_replaced_or_linked_root() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let retained = fixture.path.with_extension("retained");
    std::fs::rename(&fixture.path, &retained)?;
    std::fs::create_dir(&fixture.path)?;
    assert!(fixture.publish(ObjectId::random(), b"data").is_err());
    assert_eq!(std::fs::read_dir(&fixture.path)?.count(), 0);
    std::fs::remove_dir(&fixture.path)?;
    std::os::unix::fs::symlink(&retained, &fixture.path)?;
    assert!(fixture.publish(ObjectId::random(), b"data").is_err());
    std::fs::remove_file(&fixture.path)?;
    std::fs::rename(retained, &fixture.path)?;
    fixture.close()
}
