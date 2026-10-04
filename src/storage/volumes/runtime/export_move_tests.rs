use super::{Kind, Reply, Request, movement_tests::fixture};
use std::fs;

#[test]
fn export_retirement_waits_for_an_admitted_move_and_reclaims_its_final_copy() -> anyhow::Result<()>
{
    let fixture = fixture()?;
    assert_eq!(fixture.source.object.kind, Kind::Export);
    let root = fixture
        .source_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let id = fixture.source.object.id.clone();
    let destination = fixture.destination.path().to_path_buf();
    let original = fs::read(&fixture.source_path)?;
    fixture
        .catalog
        .handle()
        .volume_location(Request::RetireExport(id.clone()))?;
    assert!(fixture.manager.finish_export_retirement(&id).is_err());
    assert_eq!(fs::read(&fixture.source_path)?, original);
    assert!(!destination.exists());
    fixture.manager.resume_move(&fixture.job_id, || false)?;
    let Reply::Location(Some(current)) = fixture
        .catalog
        .handle()
        .volume_location(Request::Lookup(fixture.source.object.clone()))?
    else {
        anyhow::bail!("admitted move did not publish its destination");
    };
    assert_eq!(current.volume, "secondary");
    assert_eq!(fs::read(&destination)?, original);
    assert!(fixture.manager.finish_export_retirement(&id).is_err());
    assert_eq!(fs::read(&fixture.source_path)?, original);
    assert!(fixture.manager.retire_move(&fixture.job_id)?);
    assert!(!fixture.source_path.exists());
    assert!(fixture.manager.finish_export_retirement(&id)?);
    assert!(!destination.exists());
    assert_eq!(
        fixture
            .catalog
            .handle()
            .volume_location(Request::Lookup(fixture.source.object))?,
        Reply::Location(None)
    );
    let Reply::Usage(usage) = fixture.catalog.handle().volume_location(Request::Usage)? else {
        anyhow::bail!("volume usage reply is missing");
    };
    assert!(
        usage
            .iter()
            .all(|volume| volume.allocated_bytes == 0 && volume.reserved_bytes == 0)
    );
    assert!(fixture.manager.finish_export_retirement(&id)?);
    drop(fixture.destination);
    drop(fixture.manager);
    fixture.catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}
