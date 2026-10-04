use super::*;
use crate::storage::catalog::{
    RecordingCatalog,
    locations::{Location, moves::Intent},
};

const COPY_BYTES: usize = 131_072;

#[test]
fn read_only_destination_cannot_resume_a_reserved_copy() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let mut configuration = fixture.manager.inner.configuration.clone();
    configuration.volumes[1].state = VolumeState::ReadOnly;
    let manager = Manager::new(configuration, fixture.catalog.handle())?;
    assert!(manager.resume_move(&fixture.job_id, || false).is_err());
    assert!(!fixture.destination.path.exists());
    assert!(!fixture.destination.path.with_extension("tmp").exists());
    assert_source_authoritative(&fixture.catalog, &fixture.source)?;
    fixture.catalog.shutdown();
    Ok(())
}

pub(super) struct Fixture {
    pub(super) catalog: RecordingCatalog,
    pub(super) manager: Manager,
    pub(super) source: Location,
    pub(super) source_path: PathBuf,
    pub(super) job_id: String,
    pub(super) destination: Reservation,
}

pub(super) fn fixture() -> anyhow::Result<Fixture> {
    let (path, catalog, initial) = tests::fixture(4 * GROWTH_BYTES)?;
    let secondary = path.join("secondary");
    tests::create_root(&secondary)?;
    let mut configuration = initial.inner.configuration.clone();
    configuration.volumes.push(tests::volume(
        "secondary",
        secondary.clone(),
        4 * GROWTH_BYTES,
    ));
    let manager = Manager::new(configuration, catalog.handle())?;
    let (source, source_path) = source_fixture(&manager, &catalog)?;
    let job_id = uuid::Uuid::new_v4().to_string();
    let destination = destination_fixture(&manager, &catalog, &source, &secondary, &job_id)?;
    Ok(Fixture {
        catalog,
        manager,
        source,
        source_path,
        job_id,
        destination,
    })
}

fn source_fixture(
    manager: &Manager,
    catalog: &RecordingCatalog,
) -> anyhow::Result<(Location, PathBuf)> {
    let object = Object {
        kind: Kind::Export,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let reserved = manager
        .reserve(
            VolumeRole::Export,
            "camera",
            &[],
            object.clone(),
            COPY_BYTES as u64,
        )?
        .unwrap();
    let source_path = reserved.path.clone();
    let mut file = reserved.open()?;
    file.write_all(&vec![7; COPY_BYTES])?;
    let evidence = file.evidence()?;
    file.publish(evidence)?;
    drop(file);
    let Reply::Location(Some(source)) =
        catalog.handle().volume_location(Request::Lookup(object))?
    else {
        anyhow::bail!("source publication missing");
    };
    Ok((source, source_path))
}

fn destination_fixture(
    manager: &Manager,
    catalog: &RecordingCatalog,
    source: &Location,
    secondary: &Path,
    job_id: &str,
) -> anyhow::Result<Reservation> {
    let key = format!("{job_id}.mp4");
    let allocation = Allocation {
        operation: job_id.to_owned(),
        object: Object {
            kind: source.object.kind,
            id: job_id.to_owned(),
        },
        volume: "secondary".into(),
        generation: 1,
        relative_key: key.clone(),
        bytes: source.bytes,
        capacity: manager
            .inner
            .root(1)?
            .capacity(catalog.handle().volume_ledger_revision()?)?,
    };
    catalog
        .handle()
        .volume_location(Request::BeginMove(Intent {
            id: job_id.to_owned(),
            object: source.object.clone(),
            expected_revision: source.revision,
            destination: allocation,
        }))?;
    Ok(Reservation {
        inner: Arc::clone(&manager.inner),
        index: 1,
        operation: job_id.to_owned(),
        path: secondary.join(&key),
        key,
        bytes: source.bytes,
        _writer_lease: None,
    })
}

fn assert_source_authoritative(
    catalog: &RecordingCatalog,
    source: &Location,
) -> anyhow::Result<()> {
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::Lookup(source.object.clone()))?,
        Reply::Location(Some(source.clone()))
    );
    Ok(())
}

#[test]
fn move_copy_publishes_verified_destination_and_preserves_source() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let final_path = fixture.destination.path.clone();
    fixture.manager.copy_move(
        &fixture.job_id,
        &fixture.source,
        fixture.destination,
        || false,
    )?;
    let Reply::Location(Some(location)) = fixture
        .catalog
        .handle()
        .volume_location(Request::Lookup(fixture.source.object.clone()))?
    else {
        anyhow::bail!("destination publication missing");
    };
    assert_eq!(location.volume, "secondary");
    assert_eq!(location.revision, fixture.source.revision + 1);
    assert_eq!(location.digest, fixture.source.digest);
    assert_eq!(std::fs::read(final_path)?, vec![7; COPY_BYTES]);
    assert_eq!(std::fs::read(fixture.source_path)?, vec![7; COPY_BYTES]);
    let Reply::Move(job) = fixture
        .catalog
        .handle()
        .volume_location(Request::Move(fixture.job_id))?
    else {
        anyhow::bail!("move journal missing");
    };
    assert_eq!(job.phase, "published");
    fixture.catalog.shutdown();
    Ok(())
}

#[test]
fn cancelled_partial_copy_keeps_source_authority_and_recoverable_temp() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let final_path = fixture.destination.path.clone();
    let temporary = final_path.with_extension("tmp");
    let calls = Cell::new(0);
    assert!(
        fixture
            .manager
            .copy_move(
                &fixture.job_id,
                &fixture.source,
                fixture.destination,
                || {
                    calls.set(calls.get() + 1);
                    calls.get() >= 3
                }
            )
            .is_err()
    );
    assert_source_authoritative(&fixture.catalog, &fixture.source)?;
    assert!(!final_path.exists());
    assert_eq!(std::fs::metadata(temporary)?.len(), 65_536);
    assert_eq!(std::fs::read(fixture.source_path)?, vec![7; COPY_BYTES]);
    fixture.catalog.shutdown();
    Ok(())
}

#[test]
fn mismatched_source_digest_never_publishes_destination() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let final_path = fixture.destination.path.clone();
    std::fs::write(&fixture.source_path, vec![9; COPY_BYTES])?;
    assert!(
        fixture
            .manager
            .copy_move(
                &fixture.job_id,
                &fixture.source,
                fixture.destination,
                || false
            )
            .is_err()
    );
    assert_source_authoritative(&fixture.catalog, &fixture.source)?;
    assert!(!final_path.exists());
    assert_eq!(std::fs::read(fixture.source_path)?, vec![9; COPY_BYTES]);
    fixture.catalog.shutdown();
    Ok(())
}

#[test]
fn destination_collision_preserves_both_copies_and_verified_journal() -> anyhow::Result<()> {
    let fixture = fixture()?;
    let final_path = fixture.destination.path.clone();
    std::fs::write(&final_path, b"unrelated")?;
    assert!(
        fixture
            .manager
            .copy_move(
                &fixture.job_id,
                &fixture.source,
                fixture.destination,
                || false
            )
            .is_err()
    );
    assert_source_authoritative(&fixture.catalog, &fixture.source)?;
    assert_eq!(std::fs::read(final_path)?, b"unrelated");
    assert_eq!(std::fs::read(fixture.source_path)?, vec![7; COPY_BYTES]);
    let Reply::Move(job) = fixture
        .catalog
        .handle()
        .volume_location(Request::Move(fixture.job_id))?
    else {
        anyhow::bail!("move journal missing");
    };
    assert_eq!(job.phase, "verified");
    fixture.catalog.shutdown();
    Ok(())
}
