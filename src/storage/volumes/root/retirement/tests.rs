use super::*;

struct Fixture {
    path: std::path::PathBuf,
    root: Root,
    key: String,
    file_identity: String,
    digest: [u8; 32],
    operation: String,
}

impl Fixture {
    fn new() -> anyhow::Result<Self> {
        let (path, root) = super::super::file::tests::fixture()?;
        let key = format!("{}.mp4", uuid::Uuid::new_v4());
        let mut file = root.create_file(&key)?;
        file.file_mut().write_all(b"retained")?;
        let (_, file_identity, digest) = file.evidence()?;
        drop(file);
        Ok(Self {
            path,
            root,
            key,
            file_identity,
            digest,
            operation: uuid::Uuid::new_v4().to_string(),
        })
    }

    fn retire(&self) -> anyhow::Result<()> {
        self.root.retire_owned(
            &self.key,
            &self.file_identity,
            8,
            self.digest,
            &self.operation,
        )
    }

    fn prepare(&self) -> anyhow::Result<Retirement<'_>> {
        Retirement::prepare(
            &self.root,
            &self.key,
            &self.file_identity,
            8,
            self.digest,
            &self.operation,
        )
    }

    fn close(self) -> anyhow::Result<()> {
        drop(self.root);
        std::fs::remove_dir_all(self.path)?;
        Ok(())
    }
}

fn stage(retirement: &Retirement<'_>) -> anyhow::Result<()> {
    let mut source = open_removal(&retirement.root.directory, &retirement.receipt.key)?;
    retirement.verify(&mut source)?;
    rename(
        &retirement.root.directory,
        &retirement.receipt.key,
        &retirement.directory,
        &source,
    )?;
    retirement.sync()
}

#[test]
fn retirement_is_durable_and_exact_retry_reopens_receipts() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    fixture.retire()?;
    assert!(!fixture.path.join(&fixture.key).exists());
    let receipt = fixture
        .path
        .join(QUARANTINE)
        .join(&fixture.operation)
        .join(STAGED);
    assert!(receipt.is_file());
    let reopened = Root::open(&fixture.path)?;
    reopened.retire_owned(
        &fixture.key,
        &fixture.file_identity,
        8,
        fixture.digest,
        &fixture.operation,
    )?;
    drop(reopened);
    fixture.close()
}

#[test]
fn retirement_rejects_unexplained_absence_and_wrong_evidence() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    assert!(
        fixture
            .root
            .retire_owned(
                &fixture.key,
                &fixture.file_identity,
                8,
                [0; 32],
                &fixture.operation
            )
            .is_err()
    );
    assert_eq!(std::fs::read(fixture.path.join(&fixture.key))?, b"retained");
    assert!(
        fixture
            .root
            .retire_owned(
                &fixture.key,
                "wrong-file",
                8,
                fixture.digest,
                &fixture.operation
            )
            .is_err()
    );
    std::fs::remove_file(fixture.path.join(&fixture.key))?;
    assert!(fixture.retire().is_err());
    fixture.close()
}

#[test]
fn retirement_resumes_after_rename_before_staged_receipt() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let retirement = fixture.prepare()?;
    stage(&retirement)?;
    assert!(absent(&retirement.directory, STAGED)?);
    drop(retirement);
    fixture.retire()?;
    assert!(!fixture.path.join(&fixture.key).exists());
    fixture.close()
}

#[test]
fn retirement_resumes_after_unlink_only_with_staged_receipt() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let retirement = fixture.prepare()?;
    stage(&retirement)?;
    write_receipt(&retirement.directory, STAGED, &retirement.receipt)?;
    let file = open_removal(&retirement.directory, LEAF)?;
    remove(&retirement.directory, file)?;
    drop(retirement);
    fixture.retire()?;
    fixture.close()
}

#[test]
fn retirement_preserves_replacement_and_torn_receipt() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let retirement = fixture.prepare()?;
    stage(&retirement)?;
    std::fs::write(fixture.path.join(&fixture.key), b"replaced")?;
    drop(retirement);
    assert!(fixture.retire().is_err());
    assert_eq!(std::fs::read(fixture.path.join(&fixture.key))?, b"replaced");
    assert_eq!(
        std::fs::read(
            fixture
                .path
                .join(QUARANTINE)
                .join(&fixture.operation)
                .join(LEAF)
        )?,
        b"retained"
    );
    std::fs::remove_file(fixture.path.join(&fixture.key))?;
    std::fs::write(
        fixture
            .path
            .join(QUARANTINE)
            .join(&fixture.operation)
            .join(STAGED),
        b"{",
    )?;
    assert!(fixture.retire().is_err());
    assert_eq!(
        std::fs::read(
            fixture
                .path
                .join(QUARANTINE)
                .join(&fixture.operation)
                .join(LEAF)
        )?,
        b"retained"
    );
    fixture.close()
}

#[test]
fn retirement_rejects_concurrent_workers_and_changed_operation_intent() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let retirement = fixture.prepare()?;
    assert!(fixture.prepare().is_err());
    drop(retirement);
    assert!(
        fixture
            .root
            .retire_owned(
                &fixture.key,
                &fixture.file_identity,
                8,
                [0; 32],
                &fixture.operation
            )
            .is_err()
    );
    assert_eq!(std::fs::read(fixture.path.join(&fixture.key))?, b"retained");
    fixture.retire()?;
    fixture.close()
}

#[cfg(unix)]
#[test]
fn retirement_rejects_replaced_root_and_symlinked_quarantine() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let moved = fixture.path.with_extension("retained");
    std::fs::rename(&fixture.path, &moved)?;
    std::fs::create_dir(&fixture.path)?;
    assert!(fixture.retire().is_err());
    assert_eq!(std::fs::read(moved.join(&fixture.key))?, b"retained");
    std::fs::remove_dir(&fixture.path)?;
    std::fs::rename(&moved, &fixture.path)?;
    std::os::unix::fs::symlink(&fixture.path, fixture.path.join(QUARANTINE))?;
    assert!(fixture.retire().is_err());
    assert_eq!(std::fs::read(fixture.path.join(&fixture.key))?, b"retained");
    fixture.close()
}
#[test]
fn retirement_receipt_bound_preserves_existing_retry() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let retirement = fixture.prepare()?;
    for index in 1..MAX_RECEIPTS {
        retirement.staging.create_dir(format!("retained-{index}"))?;
    }
    drop(retirement);
    let next_operation = uuid::Uuid::new_v4().to_string();
    assert!(
        fixture
            .root
            .retire_owned(
                &fixture.key,
                &fixture.file_identity,
                8,
                fixture.digest,
                &next_operation
            )
            .is_err()
    );
    assert!(!fixture.path.join(QUARANTINE).join(next_operation).exists());
    assert_eq!(std::fs::read(fixture.path.join(&fixture.key))?, b"retained");
    fixture.retire()?;
    fixture.close()
}
#[test]
fn acknowledged_retirement_cleans_receipts_and_retries_without_media_deletion() -> anyhow::Result<()>
{
    let fixture = Fixture::new()?;
    fixture.retire()?;
    let acknowledge = || {
        fixture.root.acknowledge_retirement(
            &fixture.key,
            &fixture.file_identity,
            8,
            fixture.digest,
            &fixture.operation,
        )
    };
    assert!(
        fixture
            .root
            .acknowledge_retirement(
                &fixture.key,
                &fixture.file_identity,
                8,
                [0; 32],
                &fixture.operation
            )
            .is_err()
    );
    assert!(
        fixture
            .path
            .join(QUARANTINE)
            .join(&fixture.operation)
            .join(STAGED)
            .is_file()
    );
    acknowledge()?;
    assert!(
        !fixture
            .path
            .join(QUARANTINE)
            .join(&fixture.operation)
            .exists()
    );
    assert!(
        !fixture
            .path
            .join(QUARANTINE)
            .join(format!("{}.ack", fixture.operation))
            .exists()
    );
    acknowledge()?;
    fixture.close()
}

#[test]
fn acknowledgement_resumes_with_receipt_outside_partially_cleaned_directory() -> anyhow::Result<()>
{
    for remove_intent in [false, true] {
        let fixture = Fixture::new()?;
        fixture.retire()?;
        let staging = private_directory(&fixture.root.directory, OsStr::new(QUARANTINE), false)?;
        let directory = private_directory(&staging, OsStr::new(&fixture.operation), false)?;
        let receipt = read_receipt(&directory, STAGED)?.unwrap();
        let file = open_removal(&directory, STAGED)?;
        rename_receipt(
            &directory,
            &staging,
            &format!("{}.ack", fixture.operation),
            &file,
        )?;
        drop(file);
        sync_directory(&directory)?;
        sync_directory(&staging)?;
        if remove_intent {
            remove_receipt(&directory, INTENT, &receipt)?;
        }
        drop(directory);
        drop(staging);
        fixture.root.acknowledge_retirement(
            &fixture.key,
            &fixture.file_identity,
            8,
            fixture.digest,
            &fixture.operation,
        )?;
        assert!(
            !fixture
                .path
                .join(QUARANTINE)
                .join(&fixture.operation)
                .exists()
        );
        fixture.close()?;
    }
    Ok(())
}

#[test]
fn acknowledgement_preserves_unknown_files_in_the_job_directory() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    fixture.retire()?;
    let foreign = fixture
        .path
        .join(QUARANTINE)
        .join(&fixture.operation)
        .join("foreign");
    std::fs::write(&foreign, b"unknown")?;
    assert!(
        fixture
            .root
            .acknowledge_retirement(
                &fixture.key,
                &fixture.file_identity,
                8,
                fixture.digest,
                &fixture.operation
            )
            .is_err()
    );
    assert_eq!(std::fs::read(foreign)?, b"unknown");
    fixture.close()
}
#[test]
fn zero_byte_owned_temporary_retirement_still_requires_identity() -> anyhow::Result<()> {
    let (path, root) = super::super::file::tests::fixture()?;
    let key = format!("{}.tmp", uuid::Uuid::new_v4());
    let mut file = root.create_file(&key)?;
    let (bytes, identity, digest) = file.evidence()?;
    assert_eq!(bytes, 0);
    drop(file);
    let operation = uuid::Uuid::new_v4().to_string();
    assert!(
        root.retire_owned(&key, "wrong", bytes, digest, &operation)
            .is_err()
    );
    root.retire_owned(&key, &identity, bytes, digest, &operation)?;
    root.retire_owned(&key, &identity, bytes, digest, &operation)?;
    root.acknowledge_retirement(&key, &identity, bytes, digest, &operation)?;
    drop(root);
    std::fs::remove_dir_all(path)?;
    Ok(())
}

#[test]
fn bounded_absence_checks_reject_present_or_unsafe_entries() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let absent_key = format!("{}.tmp", uuid::Uuid::new_v4());
    fixture.root.confirm_absent(&[&absent_key])?;
    assert!(
        fixture
            .root
            .confirm_absent(&[&absent_key, &fixture.key])
            .is_err()
    );
    assert!(fixture.root.confirm_absent(&["../outside.mp4"]).is_err());
    assert!(
        fixture
            .root
            .confirm_absent(&[&absent_key, &absent_key, &absent_key])
            .is_err()
    );
    fixture.close()
}
