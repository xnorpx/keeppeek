use super::*;
use crate::storage::catalog::{RecordingCatalog, tests::test_dir};

#[test]
fn move_pages_are_bounded_ordered_and_keep_cancelled_work_until_cleanup() {
    let path = test_dir("volume-move-pages").join("catalog.db");
    let (catalog, intent, _) = fixture(&path);
    let handle = catalog.handle();
    handle
        .volume_location(Request::BeginMove(intent.clone()))
        .unwrap();
    handle
        .volume_location(Request::AdvanceMove(moves::Step::Cancel(intent.id.clone())))
        .unwrap();
    let query = |after, limit| {
        Request::Moves(moves::Page {
            after,
            limit,
            include_terminal: false,
        })
    };
    for limit in [0, 65, u16::MAX] {
        assert!(handle.volume_location(query(None, limit)).is_err());
    }
    let Reply::Moves(first) = handle.volume_location(query(None, 1)).unwrap() else {
        panic!("missing page");
    };
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, intent.id);
    assert!(first[0].cancellation_requested);
    assert_eq!(
        handle
            .volume_location(query(Some(first[0].id.clone()), 64))
            .unwrap(),
        Reply::Moves(vec![])
    );
    assert_eq!(
        handle.volume_location(query(Some("a".into()), 1)).unwrap(),
        Reply::Moves(first)
    );
    catalog.shutdown();
}

#[test]
fn source_retirement_releases_only_the_old_copy_after_authority_publication() {
    let path = test_dir("volume-move-retirement").join("catalog.db");
    let (catalog, intent, source) = fixture(&path);
    let handle = catalog.handle();
    handle
        .volume_location(Request::BeginMove(intent.clone()))
        .unwrap();
    let advance = |step| handle.volume_location(Request::AdvanceMove(step));
    let retired = Publication {
        operation: intent.id.clone(),
        bytes: source.bytes,
        file_identity: source.file_identity.clone(),
        digest: source.digest,
    };
    assert!(advance(moves::Step::Retiring(intent.id.clone())).is_err());
    assert!(advance(moves::Step::Retired(retired.clone())).is_err());
    advance(moves::Step::Verified(Publication {
        file_identity: "copy".into(),
        ..retired.clone()
    }))
    .unwrap();
    advance(moves::Step::FilePublished(intent.id.clone())).unwrap();
    advance(moves::Step::Publish(intent.id.clone())).unwrap();
    assert!(advance(moves::Step::Retired(retired.clone())).is_err());
    advance(moves::Step::Retiring(intent.id.clone())).unwrap();
    advance(moves::Step::Retiring(intent.id.clone())).unwrap();
    let before = handle.volume_location(Request::Usage).unwrap();
    assert!(
        advance(moves::Step::Retired(Publication {
            digest: [0; 32],
            ..retired.clone()
        }))
        .is_err()
    );
    assert_eq!(handle.volume_location(Request::Usage).unwrap(), before);
    let done = advance(moves::Step::Retired(retired.clone())).unwrap();
    assert_eq!(advance(moves::Step::Retired(retired)).unwrap(), done);
    let Reply::Usage(usage) = handle.volume_location(Request::Usage).unwrap() else {
        panic!("missing usage");
    };
    assert_eq!(
        usage.iter().map(|entry| entry.allocated_bytes).sum::<u64>(),
        source.bytes
    );
    let Reply::Location(Some(current)) = handle
        .volume_location(Request::Lookup(intent.object))
        .unwrap()
    else {
        panic!("missing destination");
    };
    assert_eq!(current.volume, "destination");
    let done = acknowledge_and_assert_pending_cleanup(&handle, &intent.id);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::Move(intent.id))
            .unwrap(),
        done
    );
    catalog.shutdown();
}

fn acknowledge_and_assert_pending_cleanup(handle: &RecordingCatalogHandle, id: &str) -> Reply {
    let Reply::Moves(pending) = handle
        .volume_location(Request::Moves(moves::Page {
            after: None,
            limit: 64,
            include_terminal: false,
        }))
        .unwrap()
    else {
        panic!("missing page");
    };
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].receipt_acknowledged);
    let done = handle
        .volume_location(Request::AdvanceMove(moves::Step::Acknowledged(id.into())))
        .unwrap();
    assert_eq!(
        handle
            .volume_location(Request::Moves(moves::Page {
                after: None,
                limit: 64,
                include_terminal: false
            }))
            .unwrap(),
        Reply::Moves(vec![])
    );
    done
}

fn allocation(handle: &RecordingCatalogHandle, operation: &str, volume: &str) -> Allocation {
    Allocation {
        operation: operation.into(),
        object: Object {
            kind: Kind::Export,
            id: operation.into(),
        },
        volume: volume.into(),
        generation: 1,
        relative_key: format!("{operation}.mp4"),
        bytes: 40,
        capacity: Capacity {
            ledger_revision: handle.volume_ledger_revision().unwrap(),
            observed_at: Instant::now(),
            available_bytes: 1_000,
            filesystem: "disk".into(),
            root_identity: volume.into(),
        },
    }
}

fn fixture(path: &std::path::Path) -> (RecordingCatalog, moves::Intent, Location) {
    let catalog = RecordingCatalog::open(path).unwrap();
    let handle = catalog.handle();
    for volume in ["source", "destination"] {
        handle
            .volume_location(Request::Bind(Binding {
                id: volume.into(),
                generation: 1,
                root: path.parent().unwrap().join(volume),
                filesystem: "disk".into(),
                root_identity: volume.into(),
                writable: true,
                draining: false,
                limit_bytes: Some(100),
                minimum_free_bytes: 10,
            }))
            .unwrap();
    }
    let source = allocation(&handle, "recording", "source");
    handle
        .volume_location(Request::Reserve(source.clone()))
        .unwrap();
    let Reply::Location(Some(location)) = handle
        .volume_location(Request::Publish(Publication {
            operation: source.operation,
            bytes: 40,
            file_identity: "source-file".into(),
            digest: [3; 32],
        }))
        .unwrap()
    else {
        panic!("missing source");
    };
    let intent = moves::Intent {
        id: "move".into(),
        object: source.object,
        expected_revision: 1,
        destination: allocation(&handle, "move", "destination"),
    };
    (catalog, intent, location)
}

#[test]
fn move_admission_preserves_source_and_recovers_exact_reservation() {
    let path = test_dir("volume-move-admission").join("catalog.db");
    let (catalog, intent, source) = fixture(&path);
    let handle = catalog.handle();
    let expected = handle
        .volume_location(Request::BeginMove(intent.clone()))
        .unwrap();
    assert!(
        handle
            .volume_location(Request::Publish(Publication {
                operation: intent.id.clone(),
                bytes: 40,
                file_identity: "copy".into(),
                digest: source.digest,
            }))
            .is_err()
    );
    assert_eq!(
        handle
            .volume_location(Request::Lookup(intent.object.clone()))
            .unwrap(),
        Reply::Location(Some(source.clone()))
    );
    assert_eq!(
        handle
            .volume_location(Request::Lookup(intent.destination.object.clone()))
            .unwrap(),
        Reply::Location(None)
    );
    assert_eq!(
        handle
            .volume_location(Request::BeginMove(intent.clone()))
            .unwrap(),
        expected
    );
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::BeginMove(intent.clone()))
            .unwrap(),
        expected
    );
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::Lookup(intent.object))
            .unwrap(),
        Reply::Location(Some(source))
    );
    catalog.shutdown();
}

#[test]
fn move_admission_rejects_stale_source_and_changed_retry_without_leaking_reservations() {
    let path = test_dir("volume-move-conflicts").join("catalog.db");
    let (catalog, mut intent, _) = fixture(&path);
    let handle = catalog.handle();
    let before = handle.volume_location(Request::Usage).unwrap();
    intent.expected_revision = 2;
    assert!(
        handle
            .volume_location(Request::BeginMove(intent.clone()))
            .is_err()
    );
    assert_eq!(handle.volume_location(Request::Usage).unwrap(), before);
    intent.expected_revision = 1;
    handle
        .volume_location(Request::BeginMove(intent.clone()))
        .unwrap();
    let before = handle.volume_location(Request::Usage).unwrap();
    intent.destination.bytes = 41;
    assert!(handle.volume_location(Request::BeginMove(intent)).is_err());
    assert_eq!(handle.volume_location(Request::Usage).unwrap(), before);
    catalog.shutdown();
}

#[test]
fn move_keeps_source_until_verified_file_publication_and_switches_once() {
    let path = test_dir("volume-move-publication").join("catalog.db");
    let (catalog, intent, source) = fixture(&path);
    let handle = catalog.handle();
    handle
        .volume_location(Request::BeginMove(intent.clone()))
        .unwrap();
    let publish = Request::AdvanceMove(moves::Step::Publish(intent.id.clone()));
    assert!(handle.volume_location(publish.clone()).is_err());
    let evidence = Publication {
        operation: intent.id.clone(),
        bytes: source.bytes,
        file_identity: "copied-file".into(),
        digest: source.digest,
    };
    let mut corrupt = evidence.clone();
    corrupt.digest = [0; 32];
    assert!(
        handle
            .volume_location(Request::AdvanceMove(moves::Step::Verified(corrupt)))
            .is_err()
    );
    let verified = Request::AdvanceMove(moves::Step::Verified(evidence));
    let result = handle.volume_location(verified.clone()).unwrap();
    assert_eq!(handle.volume_location(verified).unwrap(), result);
    assert!(handle.volume_location(publish.clone()).is_err());
    assert_eq!(
        handle
            .volume_location(Request::Lookup(intent.object.clone()))
            .unwrap(),
        Reply::Location(Some(source.clone()))
    );
    handle
        .volume_location(Request::AdvanceMove(moves::Step::FilePublished(
            intent.id.clone(),
        )))
        .unwrap();
    let result = handle.volume_location(publish.clone()).unwrap();
    assert_eq!(handle.volume_location(publish.clone()).unwrap(), result);
    assert_eq!(
        handle
            .volume_location(Request::BeginMove(intent.clone()))
            .unwrap(),
        result
    );
    let destination = assert_published_destination(&handle, &intent, &source);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    assert_eq!(catalog.handle().volume_location(publish).unwrap(), result);
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::Lookup(intent.object))
            .unwrap(),
        Reply::Location(Some(destination))
    );
    catalog.shutdown();
}

fn assert_published_destination(
    handle: &RecordingCatalogHandle,
    intent: &moves::Intent,
    source: &Location,
) -> Location {
    assert_eq!(
        handle
            .volume_location(Request::Lookup(Object {
                kind: intent.object.kind,
                id: format!("retired:{}", intent.id),
            }))
            .unwrap(),
        Reply::Location(None)
    );
    let Reply::Location(Some(destination)) = handle
        .volume_location(Request::Lookup(intent.object.clone()))
        .unwrap()
    else {
        panic!("missing destination");
    };
    assert_eq!(destination.volume, "destination");
    assert_eq!(destination.revision, source.revision + 1);
    assert_eq!(destination.digest, source.digest);
    let Reply::Usage(usage) = handle.volume_location(Request::Usage).unwrap() else {
        panic!("missing usage");
    };
    assert_eq!(
        usage.iter().map(|value| value.allocated_bytes).sum::<u64>(),
        80
    );
    assert_eq!(
        usage.iter().map(|value| value.reserved_bytes).sum::<u64>(),
        0
    );
    destination
}

#[test]
fn cancelled_move_rejects_more_writes_and_publication_without_releasing_owned_bytes() {
    let path = test_dir("volume-move-cancellation").join("catalog.db");
    let (catalog, intent, source) = fixture(&path);
    let handle = catalog.handle();
    handle
        .volume_location(Request::BeginMove(intent.clone()))
        .unwrap();
    let before = handle.volume_location(Request::Usage).unwrap();
    let cancel = Request::AdvanceMove(moves::Step::Cancel(intent.id.clone()));
    let result = handle.volume_location(cancel.clone()).unwrap();
    assert_eq!(handle.volume_location(cancel).unwrap(), result);
    let evidence = Publication {
        operation: intent.id.clone(),
        bytes: source.bytes,
        file_identity: "copy".into(),
        digest: source.digest,
    };
    assert!(
        handle
            .volume_location(Request::AdvanceMove(moves::Step::Verified(evidence)))
            .is_err()
    );
    assert!(
        handle
            .volume_location(Request::Materialize(Materialization {
                operation: intent.id.clone(),
                bytes: 0,
                file_identity: "copy".into(),
            }))
            .is_err()
    );
    assert!(
        handle
            .volume_location(Request::Grow(Growth {
                operation: intent.id,
                bytes: intent.destination.bytes,
                capacity: intent.destination.capacity,
            }))
            .is_err()
    );
    assert_eq!(handle.volume_location(Request::Usage).unwrap(), before);
    assert_eq!(
        handle
            .volume_location(Request::Lookup(intent.object))
            .unwrap(),
        Reply::Location(Some(source))
    );
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::Move("move".into()))
            .unwrap(),
        result
    );
    catalog.shutdown();
}

#[test]
fn cancellation_evidence_survives_restart_and_only_releases_the_destination() {
    let path = test_dir("volume-move-cancel-cleanup").join("catalog.db");
    let (catalog, intent, source) = fixture(&path);
    let handle = catalog.handle();
    handle
        .volume_location(Request::BeginMove(intent.clone()))
        .unwrap();
    handle
        .volume_location(Request::Materialize(Materialization {
            operation: intent.id.clone(),
            bytes: 0,
            file_identity: "copy".into(),
        }))
        .unwrap();
    let complete = Request::AdvanceMove(moves::Step::Cancelled(intent.id.clone()));
    assert!(handle.volume_location(complete.clone()).is_err());
    handle
        .volume_location(Request::AdvanceMove(moves::Step::Cancel(intent.id.clone())))
        .unwrap();
    let verify = Request::AdvanceMove(moves::Step::CancellationVerified {
        id: intent.id.clone(),
        evidence: moves::Cancellation::File {
            relative_key: "move.tmp".into(),
            bytes: 10,
            file_identity: "copy".into(),
            digest: [7; 32],
        },
    });
    let evidence = handle.volume_location(verify.clone()).unwrap();
    assert_eq!(handle.volume_location(verify.clone()).unwrap(), evidence);
    assert!(
        handle
            .volume_location(Request::AdvanceMove(moves::Step::CancellationVerified {
                id: intent.id.clone(),
                evidence: moves::Cancellation::Empty
            }))
            .is_err()
    );
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    assert_eq!(handle.volume_location(verify).unwrap(), evidence);
    let done = handle.volume_location(complete.clone()).unwrap();
    assert_eq!(handle.volume_location(complete).unwrap(), done);
    assert_eq!(
        handle
            .volume_location(Request::Lookup(intent.object))
            .unwrap(),
        Reply::Location(Some(source.clone()))
    );
    let Reply::Usage(usage) = handle.volume_location(Request::Usage).unwrap() else {
        panic!("missing usage");
    };
    assert_eq!(
        usage.iter().map(|entry| entry.allocated_bytes).sum::<u64>(),
        source.bytes
    );
    handle
        .volume_location(Request::AdvanceMove(moves::Step::Acknowledged(intent.id)))
        .unwrap();
    catalog.shutdown();
}

#[test]
fn empty_cancellation_requires_no_captured_file_and_a_durable_cancel_request() {
    let path = test_dir("volume-move-empty-cancel").join("catalog.db");
    let (catalog, intent, source) = fixture(&path);
    let handle = catalog.handle();
    handle
        .volume_location(Request::BeginMove(intent.clone()))
        .unwrap();
    let verify = Request::AdvanceMove(moves::Step::CancellationVerified {
        id: intent.id.clone(),
        evidence: moves::Cancellation::Empty,
    });
    assert!(handle.volume_location(verify.clone()).is_err());
    handle
        .volume_location(Request::AdvanceMove(moves::Step::Cancel(intent.id.clone())))
        .unwrap();
    handle.volume_location(verify).unwrap();
    handle
        .volume_location(Request::AdvanceMove(moves::Step::Cancelled(intent.id)))
        .unwrap();
    assert_eq!(
        handle
            .volume_location(Request::Lookup(intent.object))
            .unwrap(),
        Reply::Location(Some(source))
    );
    catalog.shutdown();
}
