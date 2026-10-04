use super::super::{
    movement_tests::{Fixture, fixture},
    *,
};
use super::*;

#[test]
fn read_only_destination_keeps_cancelled_copy_until_writable() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let mut output = staged(&fixture)?;
    output.write_all(&[7; 8])?;
    drop(output);
    let configuration = fixture.manager.inner.configuration.clone();
    let mut read_only = configuration.clone();
    read_only.volumes[1].state = VolumeState::ReadOnly;
    let manager = Manager::new(read_only, fixture.catalog.handle())?;
    assert!(manager.cancel_move(&fixture.job_id).is_err());
    assert_eq!(
        std::fs::read(fixture.destination.path.with_extension("tmp"))?,
        [7; 8]
    );
    let manager = Manager::new(configuration, fixture.catalog.handle())?;
    manager.cancel_move(&fixture.job_id)?;
    assert_cancelled(&fixture)?;
    fixture.catalog.shutdown();
    Ok(())
}

fn staged(fixture: &Fixture) -> anyhow::Result<ReservedFile> {
    let old = &fixture.destination;
    let reservation = Reservation {
        inner: Arc::clone(&old.inner),
        index: old.index,
        operation: old.operation.clone(),
        key: old.key.clone(),
        path: old.path.clone(),
        bytes: old.bytes,
    };
    let file = fixture
        .manager
        .inner
        .root(old.index)?
        .create_file(&format!("{}.tmp", fixture.job_id))?;
    let mut output = ReservedFile {
        reservation,
        file,
        evidence: RefCell::new(None),
        published: Cell::new(false),
        failed: false,
    };
    output.checkpoint()?;
    Ok(output)
}

fn assert_cancelled(fixture: &Fixture) -> anyhow::Result<()> {
    let Reply::Move(job) = fixture
        .catalog
        .handle()
        .volume_location(Request::Move(fixture.job_id.clone()))?
    else {
        anyhow::bail!("missing move");
    };
    assert_eq!(job.phase, "cancelled");
    assert!(job.receipt_acknowledged);
    assert_eq!(
        fixture
            .catalog
            .handle()
            .volume_location(Request::Lookup(fixture.source.object.clone()))?,
        Reply::Location(Some(fixture.source.clone()))
    );
    assert_eq!(std::fs::read(&fixture.source_path)?, vec![7; 131_072]);
    assert!(!fixture.destination.path.exists());
    assert!(!fixture.destination.path.with_extension("tmp").exists());
    fixture.manager.finish_cancelled_move(&fixture.job_id)?;
    Ok(())
}

#[test]
fn cancellation_removes_only_owned_empty_or_partial_targets() -> anyhow::Result<()> {
    for size in [None, Some(0), Some(65_536)] {
        let fixture = fixture()?;
        if let Some(size) = size {
            let mut output = staged(&fixture)?;
            output.write_all(&vec![7; size])?;
        }
        fixture.manager.cancel_move(&fixture.job_id)?;
        assert_cancelled(&fixture)?;
        fixture.manager.cancel_move(&fixture.job_id)?;
        assert_cancelled(&fixture)?;
        fixture.catalog.shutdown();
    }
    Ok(())
}

#[test]
fn cancellation_latches_while_worker_busy_and_preserves_ambiguous_temp() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let lease = fixture
        .catalog
        .handle()
        .claim_volume_move(&fixture.job_id)?;
    assert!(fixture.manager.cancel_move(&fixture.job_id).is_err());
    drop(lease);
    let temporary = fixture.destination.path.with_extension("tmp");
    std::fs::write(&temporary, b"unowned")?;
    assert!(
        fixture
            .manager
            .finish_cancelled_move(&fixture.job_id)
            .is_err()
    );
    assert_eq!(std::fs::read(temporary)?, b"unowned");
    assert!(fixture.source_path.exists());
    let Reply::Move(job) = fixture
        .catalog
        .handle()
        .volume_location(Request::Move(fixture.job_id))?
    else {
        anyhow::bail!("missing move");
    };
    assert!(job.cancellation_requested);
    assert_eq!(job.phase, "reserved");
    fixture.catalog.shutdown();
    Ok(())
}

#[test]
fn cancellation_recovers_every_verified_target_name() -> anyhow::Result<()> {
    for phase in ["verified_temp", "verified_final", "file_published"] {
        let fixture = fixture()?;
        let mut output = staged(&fixture)?;
        output.write_all(&vec![7; 131_072])?;
        let evidence = output.evidence()?;
        fixture
            .catalog
            .handle()
            .volume_location(Request::AdvanceMove(Step::Verified(evidence)))?;
        if phase == "verified_temp" {
            drop(output);
        } else {
            output.file.publish_staged(&fixture.destination.key)?;
        }
        if phase == "file_published" {
            fixture
                .catalog
                .handle()
                .volume_location(Request::AdvanceMove(Step::FilePublished(
                    fixture.job_id.clone(),
                )))?;
        }
        fixture.manager.cancel_move(&fixture.job_id)?;
        assert_cancelled(&fixture)?;
        fixture.catalog.shutdown();
    }
    Ok(())
}

#[test]
fn cancellation_recovers_after_target_removed_before_catalog_commit() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let mut output = staged(&fixture)?;
    output.write_all(&vec![7; 1024])?;
    drop(output);
    fixture
        .catalog
        .handle()
        .volume_location(Request::AdvanceMove(Step::Cancel(fixture.job_id.clone())))?;
    let Reply::Move(job) = fixture
        .catalog
        .handle()
        .volume_location(Request::Move(fixture.job_id.clone()))?
    else {
        anyhow::bail!("missing move");
    };
    let root = fixture.manager.inner.root(1)?;
    let evidence = capture_target(root, &job)?;
    fixture
        .catalog
        .handle()
        .volume_location(Request::AdvanceMove(Step::CancellationVerified {
            id: job.id.clone(),
            evidence: evidence.clone(),
        }))?;
    let Cancellation::File {
        relative_key,
        bytes,
        file_identity,
        digest,
    } = evidence
    else {
        anyhow::bail!("missing file evidence");
    };
    root.retire_owned(&relative_key, &file_identity, bytes, digest, &job.id)?;
    assert!(!fixture.destination.path.with_extension("tmp").exists());
    fixture.manager.finish_cancelled_move(&job.id)?;
    assert_cancelled(&fixture)?;
    fixture.catalog.shutdown();
    Ok(())
}
