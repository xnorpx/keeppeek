use super::{
    Allocation, Binding, Capacity, Kind, Materialization, Object, Publication, Reply, Request,
};
use crate::storage::catalog::{RecordingCatalog, RecordingCatalogHandle, tests::test_dir};
use std::time::Instant;

fn fixture(name: &str) -> anyhow::Result<(RecordingCatalog, Binding)> {
    let root = test_dir(name);
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
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
    Ok((catalog, binding))
}

fn guard(handle: &RecordingCatalogHandle, allowed: bool) -> anyhow::Result<()> {
    let revision = handle.volume_ledger_revision()?;
    let before = handle.volume_location(Request::Usage)?;
    let result = handle.volume_location(Request::EnsureRemovable("primary".into()));
    if allowed {
        assert_eq!(result?, Reply::Bound);
    } else {
        assert!(result.is_err());
    }
    assert_eq!(handle.volume_ledger_revision()?, revision);
    assert_eq!(handle.volume_location(Request::Usage)?, before);
    Ok(())
}

fn drain(handle: &RecordingCatalogHandle) -> anyhow::Result<()> {
    assert_eq!(
        handle.volume_location(Request::SetDraining {
            volume: "primary".into(),
            generation: 1,
            draining: true,
        })?,
        Reply::Bound
    );
    Ok(())
}

fn reserve_for_removal(
    handle: &RecordingCatalogHandle,
    kind: Kind,
    operation: &str,
) -> anyhow::Result<()> {
    let extension = if kind == Kind::Thumbnail {
        "jpg"
    } else {
        "mp4"
    };
    handle.volume_location(Request::Reserve(Allocation {
        operation: operation.into(),
        object: Object {
            kind,
            id: operation.into(),
        },
        volume: "primary".into(),
        generation: 1,
        relative_key: format!("{operation}.{extension}"),
        bytes: 10,
        capacity: Capacity {
            ledger_revision: handle.volume_ledger_revision()?,
            observed_at: Instant::now(),
            available_bytes: 1000,
            filesystem: "disk".into(),
            root_identity: "root-primary".into(),
        },
    }))?;
    Ok(())
}

#[test]
fn unbound_definition_is_removable_without_creating_catalog_ownership() -> anyhow::Result<()> {
    let (catalog, binding) = fixture("removal-unbound")?;
    guard(&catalog.handle(), true)?;
    guard(&catalog.handle(), true)?;
    assert_eq!(
        catalog.handle().volume_location(Request::Usage)?,
        Reply::Usage(vec![])
    );
    assert!(!binding.root.exists());
    catalog.shutdown();
    Ok(())
}

#[test]
fn bound_empty_definition_requires_effective_drain_and_keeps_its_binding() -> anyhow::Result<()> {
    let (catalog, binding) = fixture("removal-empty-bound")?;
    let handle = catalog.handle();
    handle.volume_location(Request::Bind(binding.clone()))?;
    guard(&handle, false)?;
    drain(&handle)?;
    guard(&handle, true)?;
    guard(&handle, true)?;
    let mut replacement = binding;
    replacement.root_identity = "different-root".into();
    assert!(handle.volume_location(Request::Bind(replacement)).is_err());
    drop(handle);
    catalog.shutdown();
    Ok(())
}

#[test]
fn configured_drain_alone_allows_empty_definition_removal() -> anyhow::Result<()> {
    let (catalog, mut binding) = fixture("removal-configured-drain")?;
    binding.draining = true;
    catalog.handle().volume_location(Request::Bind(binding))?;
    guard(&catalog.handle(), true)?;
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        anyhow::bail!("usage reply missing")
    };
    assert_eq!(usage.len(), 1);
    assert!(usage[0].configured_draining);
    assert!(!usage[0].operator_draining);
    catalog.shutdown();
    Ok(())
}

#[test]
fn reservation_and_publication_block_removal_after_drain() -> anyhow::Result<()> {
    let (catalog, binding) = fixture("removal-owned-export")?;
    let handle = catalog.handle();
    handle.volume_location(Request::Bind(binding))?;
    reserve_for_removal(&handle, Kind::Export, "export")?;
    drain(&handle)?;
    guard(&handle, false)?;
    let object = Object {
        kind: Kind::Export,
        id: "export".into(),
    };
    let published = handle.volume_location(Request::Publish(Publication {
        operation: "export".into(),
        bytes: 10,
        file_identity: "file".into(),
        digest: [7; 32],
    }))?;
    assert!(matches!(published, Reply::Location(Some(_))));
    guard(&handle, false)?;
    assert_eq!(handle.volume_location(Request::Lookup(object))?, published);
    drop(handle);
    catalog.shutdown();
    Ok(())
}

fn pending_image_receipt(
    handle: &RecordingCatalogHandle,
    evidence: &Publication,
) -> anyhow::Result<Reply> {
    let reply = handle.volume_location(Request::ImageRetirement(evidence.operation.clone()))?;
    let Reply::ImageRetirement(Some(job)) = &reply else {
        anyhow::bail!("retired image receipt missing")
    };
    assert!(job.complete);
    assert!(!job.acknowledged);
    assert_eq!(job.location.bytes, evidence.bytes);
    assert_eq!(job.location.file_identity, evidence.file_identity);
    assert_eq!(job.location.digest, evidence.digest);
    let Reply::Usage(usage) = handle.volume_location(Request::Usage)? else {
        anyhow::bail!("usage reply missing")
    };
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].allocated_bytes, 0);
    assert_eq!(usage[0].reserved_bytes, 0);
    Ok(reply)
}

#[test]
fn retired_image_blocks_zero_usage_volume_removal_until_acknowledged() -> anyhow::Result<()> {
    let (catalog, binding) = fixture("removal-image-receipt")?;
    let path = binding.root.parent().unwrap().join("catalog.db");
    let handle = catalog.handle();
    handle.volume_location(Request::Bind(binding))?;
    reserve_for_removal(&handle, Kind::Thumbnail, "image")?;
    let evidence = Publication {
        operation: "image".into(),
        bytes: 10,
        file_identity: "image-file".into(),
        digest: [9; 32],
    };
    handle.volume_location(Request::Materialize(Materialization {
        operation: evidence.operation.clone(),
        bytes: evidence.bytes,
        file_identity: evidence.file_identity.clone(),
    }))?;
    drain(&handle)?;
    assert_eq!(
        handle.volume_location(Request::ImageAbandoned(evidence.clone()))?,
        Reply::Bound
    );
    assert_eq!(
        handle.volume_location(Request::ImageRetired(evidence.clone()))?,
        Reply::Bound
    );
    let receipt = pending_image_receipt(&handle, &evidence)?;
    guard(&handle, false)?;
    drop(handle);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path)?;
    let handle = catalog.handle();
    assert_eq!(pending_image_receipt(&handle, &evidence)?, receipt);
    guard(&handle, false)?;
    assert_eq!(
        handle.volume_location(Request::ImageRetired(evidence.clone()))?,
        Reply::Bound
    );
    assert_eq!(pending_image_receipt(&handle, &evidence)?, receipt);
    assert_eq!(
        handle.volume_location(Request::ImageRetirementAcknowledged(
            evidence.operation.clone()
        ))?,
        Reply::Bound
    );
    guard(&handle, true)?;
    let Reply::ImageRetirement(Some(acknowledged)) =
        handle.volume_location(Request::ImageRetirement(evidence.operation))?
    else {
        anyhow::bail!("acknowledged image receipt missing")
    };
    assert!(acknowledged.complete && acknowledged.acknowledged);
    assert_eq!(acknowledged.location.digest, evidence.digest);
    drop(handle);
    catalog.shutdown();
    Ok(())
}

fn reserve_unbound_archive_candidate(
    handle: &RecordingCatalogHandle,
    candidate: &std::path::Path,
) -> anyhow::Result<String> {
    use super::archives;
    let id = uuid::Uuid::new_v4().to_string();
    let intent = archives::Intent {
        id: id.clone(),
        policy: archives::Policy {
            source: "camera".into(),
            groups: vec![],
            configuration: serde_json::from_value(serde_json::json!({
                "volumes": [{"id":"primary","root":candidate,"roles":["archive"],"state":"disabled"}],
                "placement": [{"role":"archive","candidates":["primary"]}]
            }))?,
        },
    };
    let allocation = Allocation {
        operation: "source-recording".into(),
        object: Object {
            kind: Kind::Recording,
            id: "source-recording".into(),
        },
        volume: "source".into(),
        generation: 1,
        relative_key: "recording.mp4".into(),
        bytes: 10,
        capacity: Capacity {
            ledger_revision: handle.volume_ledger_revision()?,
            observed_at: Instant::now(),
            available_bytes: 1000,
            filesystem: "disk".into(),
            root_identity: "source-root".into(),
        },
    };
    assert_eq!(
        handle.volume_location(Request::ReserveArchive(allocation, intent))?,
        Reply::Reserved {
            operation: "source-recording".into(),
            bytes: 10
        }
    );
    Ok(id)
}

#[test]
fn unbound_archive_candidate_cannot_be_removed_while_source_reservation_is_live()
-> anyhow::Result<()> {
    use super::recording_recovery::Action;
    let (catalog, candidate) = fixture("removal-unbound-archive-candidate")?;
    let path = candidate.root.parent().unwrap().join("catalog.db");
    let handle = catalog.handle();
    let mut source = candidate.clone();
    source.id = "source".into();
    source.root = candidate.root.parent().unwrap().join("source");
    source.root_identity = "source-root".into();
    handle.volume_location(Request::Bind(source))?;
    guard(&handle, true)?;
    let archive_id = reserve_unbound_archive_candidate(&handle, &candidate.root)?;
    assert_eq!(
        handle.volume_location(Request::Archive(archive_id.clone()))?,
        Reply::Archive(None)
    );
    let Reply::Usage(usage) = handle.volume_location(Request::Usage)? else {
        anyhow::bail!("usage missing")
    };
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].volume, "source");
    guard(&handle, false)?;
    drop(handle);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path)?;
    let handle = catalog.handle();
    guard(&handle, false)?;
    let Reply::PendingRecording(Some(pending)) = handle.volume_location(
        Request::RecordingRecovery(Action::Load("source-recording".into())),
    )?
    else {
        anyhow::bail!("unopened source reservation missing")
    };
    assert_eq!(
        handle.volume_location(Request::RecordingRecovery(Action::ReleaseUnopened(pending)))?,
        Reply::Bound
    );
    guard(&handle, true)?;
    assert_eq!(
        handle.volume_location(Request::Archive(archive_id))?,
        Reply::Archive(None)
    );
    let Reply::Usage(usage) = handle.volume_location(Request::Usage)? else {
        anyhow::bail!("usage missing")
    };
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].volume, "source");
    assert_eq!(usage[0].allocated_bytes, 0);
    assert!(!candidate.root.exists());
    drop(handle);
    catalog.shutdown();
    Ok(())
}
