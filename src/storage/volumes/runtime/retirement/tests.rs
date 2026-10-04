use super::*;
use crate::storage::volumes::runtime::movement_tests::fixture;

#[test]
fn retirement_removes_only_the_verified_old_copy_and_retries_after_restart() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let target = fixture.destination.path.clone();
    let configuration = fixture.manager.inner.configuration.clone();
    fixture.manager.copy_move(
        &fixture.job_id,
        &fixture.source,
        fixture.destination,
        || false,
    )?;
    assert!(fixture.source_path.exists());
    assert!(fixture.manager.retire_move(&fixture.job_id)?);
    assert!(!fixture.source_path.exists());
    assert!(target.exists());
    assert!(fixture.manager.retire_move(&fixture.job_id)?);
    let Reply::Usage(usage) = fixture.catalog.handle().volume_location(Request::Usage)? else {
        anyhow::bail!("missing usage");
    };
    assert_eq!(
        usage.iter().map(|entry| entry.allocated_bytes).sum::<u64>(),
        fixture.source.bytes
    );
    let catalog_path = fixture
        .source_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("catalog.db");
    drop(fixture.manager);
    fixture.catalog.shutdown();
    let catalog = crate::storage::catalog::RecordingCatalog::open(&catalog_path)?;
    let manager = Manager::new(configuration, catalog.handle())?;
    assert!(manager.retire_move(&fixture.job_id)?);
    catalog.shutdown();
    Ok(())
}

#[test]
fn damaged_destination_cannot_authorize_source_retirement() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let target = fixture.destination.path.clone();
    fixture.manager.copy_move(
        &fixture.job_id,
        &fixture.source,
        fixture.destination,
        || false,
    )?;
    std::fs::write(&target, vec![8; fixture.source.bytes as usize])?;
    assert!(fixture.manager.retire_move(&fixture.job_id).is_err());
    assert!(fixture.source_path.exists());
    let Reply::Move(job) = fixture
        .catalog
        .handle()
        .volume_location(Request::Move(fixture.job_id))?
    else {
        anyhow::bail!("missing job");
    };
    assert_eq!(job.phase, "published");
    fixture.catalog.shutdown();
    Ok(())
}
