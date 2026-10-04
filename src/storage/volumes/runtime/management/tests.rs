use super::*;
use crate::storage::RecordingCatalog;

fn fixture() -> anyhow::Result<(PathBuf, RecordingCatalog, Manager, Object)> {
    let (path, catalog, initial) = super::super::tests::fixture(4 * GROWTH_BYTES)?;
    let secondary = path.join("secondary");
    super::super::tests::create_root(&secondary)?;
    let mut configuration = initial.inner.configuration.clone();
    configuration.volumes.push(super::super::tests::volume(
        "secondary",
        secondary,
        GROWTH_BYTES,
    ));
    let manager = Manager::new(configuration, catalog.handle())?;
    let object = Object {
        kind: Kind::Export,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let mut file = manager
        .reserve(VolumeRole::Export, "camera", &[], object.clone(), 8)?
        .unwrap()
        .open()?;
    file.write_all(b"12345678")?;
    let evidence = file.evidence()?;
    file.publish(evidence)?;
    Ok((path, catalog, manager, object))
}

fn request() -> PlacementRequest<'static> {
    PlacementRequest {
        role: VolumeRole::Export,
        source: "camera",
        group: "",
        required_bytes: 8,
    }
}

#[test]
fn object_pages_exclude_unpublished_and_retired_move_sources() -> anyhow::Result<()> {
    use crate::storage::catalog::locations::objects::Page;
    let (path, catalog, manager, object) = fixture()?;
    let pending = Object {
        kind: Kind::Export,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let _reservation = manager.reserve(VolumeRole::Export, "camera", &[], pending, 8)?;
    let page = |volume: &str, after, limit| {
        catalog.handle().volume_location(Request::Objects(Page {
            volume: volume.into(),
            after,
            limit,
        }))
    };
    let Reply::Objects(first) = page("primary", None, 1)? else {
        panic!("object page missing")
    };
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].object, object);
    assert_eq!(
        page("primary", Some(object.clone()), 1)?,
        Reply::Objects(vec![])
    );
    assert!(page("primary", None, 65).is_err());
    let preview = manager.preview_move(object.clone(), "secondary", &request(), &[])?;
    let id = uuid::Uuid::new_v4().to_string();
    manager.admit_move(&id, &preview)?;
    manager.resume_move(&id, || false)?;
    assert_eq!(page("primary", None, 64)?, Reply::Objects(vec![]));
    let Reply::Objects(moved) = page("secondary", None, 64)? else {
        panic!("object page missing")
    };
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].object, object);
    assert!(manager.retire_move(&id)?);
    drop(_reservation);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(path)?;
    Ok(())
}

#[test]
fn confirmed_move_is_journaled_without_copy_and_retries_after_completion() -> anyhow::Result<()> {
    let (path, catalog, manager, object) = fixture()?;
    let before = catalog.handle().volume_ledger_revision()?;
    let preview = manager.preview_move(object.clone(), "secondary", &request(), &[])?;
    assert_eq!(catalog.handle().volume_ledger_revision()?, before);
    assert_eq!(std::fs::read_dir(path.join("secondary"))?.count(), 0);
    let id = uuid::Uuid::new_v4().to_string();
    manager.admit_move(&id, &preview)?;
    assert_eq!(std::fs::read_dir(path.join("secondary"))?.count(), 0);
    manager.admit_move(&id, &preview)?;
    manager.resume_move(&id, || false)?;
    assert!(manager.retire_move(&id)?);
    manager.admit_move(&id, &preview)?;
    assert!(
        manager
            .admit_move(&uuid::Uuid::new_v4().to_string(), &preview)
            .is_err()
    );
    let Reply::Location(Some(current)) =
        catalog.handle().volume_location(Request::Lookup(object))?
    else {
        panic!("location is missing")
    };
    assert_eq!(current.volume, "secondary");
    assert_eq!(std::fs::read(manager.owned_path(&current)?)?, b"12345678");
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(path)?;
    Ok(())
}

#[test]
fn move_confirmation_rechecks_destination_state_and_capacity() -> anyhow::Result<()> {
    let (path, catalog, manager, object) = fixture()?;
    let preview = manager.preview_move(object.clone(), "secondary", &request(), &[])?;
    for state in [
        VolumeState::ReadOnly,
        VolumeState::Draining,
        VolumeState::Disabled,
    ] {
        let mut configuration = manager.inner.configuration.clone();
        configuration.volumes[1].state = state;
        let changed = Manager::new(configuration, catalog.handle())?;
        assert!(
            changed
                .admit_move(&uuid::Uuid::new_v4().to_string(), &preview)
                .is_err()
        );
    }
    let mut configuration = manager.inner.configuration.clone();
    configuration.volumes[1].capacity_bytes = Some(1);
    let changed = Manager::new(configuration, catalog.handle())?;
    assert!(
        changed
            .admit_move(&uuid::Uuid::new_v4().to_string(), &preview)
            .is_err()
    );
    assert_eq!(
        catalog.handle().volume_location(Request::Lookup(object))?,
        Reply::Location(Some(preview.source))
    );
    assert_eq!(std::fs::read_dir(path.join("secondary"))?.count(), 0);
    drop(changed);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(path)?;
    Ok(())
}

#[test]
fn move_preview_rejects_incompatible_roles_and_source_allowlists() -> anyhow::Result<()> {
    let (path, catalog, manager, object) = fixture()?;
    let mut incompatible = request();
    incompatible.role = VolumeRole::Active;
    assert!(
        manager
            .preview_move(object.clone(), "secondary", &incompatible, &[])
            .is_err()
    );
    let mut configuration = manager.inner.configuration.clone();
    configuration.volumes[1].sources = vec!["other-camera".into()];
    let changed = Manager::new(configuration, catalog.handle())?;
    assert!(
        changed
            .preview_move(object, "secondary", &request(), &[])
            .is_err()
    );
    drop(changed);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(path)?;
    Ok(())
}
