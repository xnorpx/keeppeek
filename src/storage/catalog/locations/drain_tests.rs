use super::{Allocation, Binding, Capacity, Growth, Kind, Object, Reply, Request, Usage};
use crate::storage::catalog::{RecordingCatalog, RecordingCatalogHandle, tests::test_dir};
use std::{path::PathBuf, time::Instant};

fn fixture(name: &str) -> anyhow::Result<(PathBuf, RecordingCatalog, Binding)> {
    let root = test_dir(name);
    let path = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&path)?;
    let binding = Binding {
        id: "primary".into(),
        generation: 1,
        root: root.join("primary"),
        filesystem: "disk".into(),
        root_identity: "root-primary".into(),
        writable: true,
        draining: false,
        limit_bytes: Some(100),
        minimum_free_bytes: 0,
    };
    catalog
        .handle()
        .volume_location(Request::Bind(binding.clone()))?;
    Ok((path, catalog, binding))
}

fn allocation(handle: &RecordingCatalogHandle, operation: &str) -> anyhow::Result<Allocation> {
    Ok(Allocation {
        operation: operation.into(),
        object: Object {
            kind: Kind::Recording,
            id: operation.into(),
        },
        volume: "primary".into(),
        generation: 1,
        relative_key: format!("{operation}.mp4"),
        bytes: 10,
        capacity: Capacity {
            ledger_revision: handle.volume_ledger_revision()?,
            observed_at: Instant::now(),
            available_bytes: 1000,
            filesystem: "disk".into(),
            root_identity: "root-primary".into(),
        },
    })
}

fn usage(handle: &RecordingCatalogHandle) -> anyhow::Result<Usage> {
    let Reply::Usage(mut usage) = handle.volume_location(Request::Usage)? else {
        anyhow::bail!("volume usage reply missing")
    };
    assert_eq!(usage.len(), 1);
    Ok(usage.remove(0))
}

fn set_draining(handle: &RecordingCatalogHandle, draining: bool) -> anyhow::Result<()> {
    assert_eq!(
        handle.volume_location(Request::SetDraining {
            volume: "primary".into(),
            generation: 1,
            draining,
        })?,
        Reply::Bound
    );
    Ok(())
}

#[test]
fn operator_drain_survives_configuration_rebind_and_catalog_restart() -> anyhow::Result<()> {
    let (path, catalog, binding) = fixture("operator-drain-restart")?;
    let handle = catalog.handle();
    handle.volume_location(Request::Reserve(allocation(&handle, "existing")?))?;
    let stale = allocation(&handle, "stale-new")?;
    set_draining(&handle, true)?;
    let drained_revision = handle.volume_ledger_revision()?;
    assert!(handle.volume_location(Request::Reserve(stale)).is_err());
    assert_eq!(handle.volume_ledger_revision()?, drained_revision);
    handle.volume_location(Request::Bind(binding.clone()))?;
    assert!(usage(&handle)?.operator_draining);
    assert!(!usage(&handle)?.configured_draining);
    drop(handle);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path)?;
    let handle = catalog.handle();
    handle.volume_location(Request::Bind(binding))?;
    let fresh = allocation(&handle, "fresh-new")?;
    let before = usage(&handle)?;
    assert!(before.operator_draining);
    assert!(!before.configured_draining);
    assert!(
        handle
            .volume_location(Request::Reserve(fresh.clone()))
            .is_err()
    );
    assert_eq!(usage(&handle)?, before);
    assert_eq!(
        handle.volume_location(Request::Grow(Growth {
            operation: "existing".into(),
            bytes: 20,
            capacity: fresh.capacity,
        }))?,
        Reply::Reserved {
            operation: "existing".into(),
            bytes: 20
        }
    );
    assert_eq!(usage(&handle)?.allocated_bytes, 20);
    assert_eq!(usage(&handle)?.reserved_bytes, 20);
    assert!(usage(&handle)?.operator_draining);
    drop(handle);
    catalog.shutdown();
    Ok(())
}

#[test]
fn repeated_operator_drain_commands_do_not_invalidate_capacity_observations() -> anyhow::Result<()>
{
    let (_, catalog, _) = fixture("operator-drain-idempotent")?;
    let handle = catalog.handle();
    for draining in [true, false] {
        let previous = handle.volume_ledger_revision()?;
        set_draining(&handle, draining)?;
        let changed = handle.volume_ledger_revision()?;
        assert!(changed > previous);
        let before = usage(&handle)?;
        set_draining(&handle, draining)?;
        assert_eq!(handle.volume_ledger_revision()?, changed);
        assert_eq!(usage(&handle)?, before);
    }
    let fresh = allocation(&handle, "after-clear")?;
    set_draining(&handle, false)?;
    assert_eq!(
        handle.volume_location(Request::Reserve(fresh))?,
        Reply::Reserved {
            operation: "after-clear".into(),
            bytes: 10
        }
    );
    drop(handle);
    catalog.shutdown();
    Ok(())
}

#[test]
fn operator_drain_rejects_unknown_volume_or_wrong_generation_without_mutation() -> anyhow::Result<()>
{
    let (_, catalog, _) = fixture("operator-drain-generation")?;
    let handle = catalog.handle();
    set_draining(&handle, true)?;
    let revision = handle.volume_ledger_revision()?;
    let before = usage(&handle)?;
    for (volume, generation) in [("missing", 1), ("primary", 0), ("primary", 2)] {
        for draining in [false, true] {
            assert!(
                handle
                    .volume_location(Request::SetDraining {
                        volume: volume.into(),
                        generation,
                        draining,
                    })
                    .is_err()
            );
            assert_eq!(handle.volume_ledger_revision()?, revision);
            assert_eq!(usage(&handle)?, before);
        }
    }
    drop(handle);
    catalog.shutdown();
    Ok(())
}

#[test]
fn clearing_operator_drain_cannot_override_configured_drain() -> anyhow::Result<()> {
    let (_, catalog, mut binding) = fixture("operator-drain-configured")?;
    let handle = catalog.handle();
    binding.draining = true;
    handle.volume_location(Request::Bind(binding.clone()))?;
    set_draining(&handle, true)?;
    set_draining(&handle, false)?;
    let before = usage(&handle)?;
    assert!(before.configured_draining);
    assert!(!before.operator_draining);
    let revision = handle.volume_ledger_revision()?;
    assert!(
        handle
            .volume_location(Request::Reserve(allocation(&handle, "blocked")?))
            .is_err()
    );
    assert_eq!(handle.volume_ledger_revision()?, revision);
    assert_eq!(usage(&handle)?, before);
    binding.draining = false;
    handle.volume_location(Request::Bind(binding))?;
    assert!(!usage(&handle)?.configured_draining);
    assert!(!usage(&handle)?.operator_draining);
    assert_eq!(
        handle.volume_location(Request::Reserve(allocation(&handle, "admitted")?))?,
        Reply::Reserved {
            operation: "admitted".into(),
            bytes: 10
        }
    );
    drop(handle);
    catalog.shutdown();
    Ok(())
}
