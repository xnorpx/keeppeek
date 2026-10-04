use super::super::movement_tests::{Fixture, fixture};
use super::*;

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
        .root(reservation.index)?
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

fn authoritative(fixture: &Fixture) -> anyhow::Result<Location> {
    let Reply::Location(Some(location)) = fixture
        .catalog
        .handle()
        .volume_location(Request::Lookup(fixture.source.object.clone()))?
    else {
        anyhow::bail!("authoritative location missing");
    };
    Ok(location)
}

fn assert_recovered(fixture: &Fixture) -> anyhow::Result<()> {
    fixture.manager.resume_move(&fixture.job_id, || false)?;
    let location = authoritative(fixture)?;
    assert_eq!(location.volume, "secondary");
    assert_eq!(location.digest, fixture.source.digest);
    assert_eq!(std::fs::read(&fixture.source_path)?, vec![7; 131_072]);
    assert_eq!(std::fs::read(&fixture.destination.path)?, vec![7; 131_072]);
    fixture.manager.resume_move(&fixture.job_id, || false)?;
    assert_eq!(authoritative(fixture)?, location);
    Ok(())
}

#[test]
fn recovery_appends_uncheckpointed_tail_and_is_idempotent() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let mut output = staged(&fixture)?;
    output.write_all(&vec![7; 65_536])?;
    drop(output);
    assert_recovered(&fixture)?;
    fixture.catalog.shutdown();
    Ok(())
}

#[test]
fn recovery_survives_catalog_and_manager_restart() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let mut output = staged(&fixture)?;
    output.write_all(&vec![7; 65_536])?;
    output.checkpoint()?;
    output.write_all(&vec![7; 1_024])?;
    drop(output);
    let configuration = fixture.manager.inner.configuration.clone();
    let catalog_path = fixture
        .source_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("catalog.db");
    let job_id = fixture.job_id;
    let object = fixture.source.object;
    let source_path = fixture.source_path;
    drop(fixture.manager);
    drop(fixture.destination);
    fixture.catalog.shutdown();
    let catalog = crate::storage::catalog::RecordingCatalog::open(&catalog_path)?;
    let manager = Manager::new(configuration, catalog.handle())?;
    manager.resume_move(&job_id, || false)?;
    let Reply::Location(Some(location)) =
        catalog.handle().volume_location(Request::Lookup(object))?
    else {
        anyhow::bail!("restarted publication missing");
    };
    assert_eq!(location.volume, "secondary");
    assert_eq!(std::fs::read(source_path)?, vec![7; 131_072]);
    catalog.shutdown();
    Ok(())
}

#[test]
fn recovery_handles_verified_temp_renamed_temp_and_published_file() -> anyhow::Result<()> {
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
        assert_recovered(&fixture)?;
        fixture.catalog.shutdown();
    }
    Ok(())
}

#[test]
fn recovery_preserves_ambiguous_unclaimed_temp_and_changed_prefix() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let temporary = fixture.destination.path.with_extension("tmp");
    std::fs::write(&temporary, [])?;
    assert!(
        fixture
            .manager
            .resume_move(&fixture.job_id, || false)
            .is_err()
    );
    assert_eq!(std::fs::metadata(temporary)?.len(), 0);
    assert_eq!(authoritative(&fixture)?, fixture.source);
    fixture.catalog.shutdown();

    let fixture = self::fixture()?;
    let mut output = staged(&fixture)?;
    output.write_all(&vec![9; 65_536])?;
    drop(output);
    assert!(
        fixture
            .manager
            .resume_move(&fixture.job_id, || false)
            .is_err()
    );
    assert_eq!(authoritative(&fixture)?, fixture.source);
    assert!(!fixture.destination.path.exists());
    fixture.catalog.shutdown();
    Ok(())
}

#[test]
fn recovery_refuses_durable_cancellation_and_a_second_worker() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let lease = fixture
        .catalog
        .handle()
        .claim_volume_move(&fixture.job_id)?;
    assert!(
        fixture
            .manager
            .resume_move(&fixture.job_id, || false)
            .is_err()
    );
    drop(lease);
    let mut output = staged(&fixture)?;
    output.write_all(b"partial")?;
    drop(output);
    fixture
        .catalog
        .handle()
        .volume_location(Request::AdvanceMove(Step::Cancel(fixture.job_id.clone())))?;
    assert!(
        fixture
            .manager
            .resume_move(&fixture.job_id, || false)
            .is_err()
    );
    assert_eq!(
        std::fs::read(fixture.destination.path.with_extension("tmp"))?,
        b"partial"
    );
    assert_eq!(authoritative(&fixture)?, fixture.source);
    fixture.catalog.shutdown();
    Ok(())
}
