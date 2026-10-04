use super::{Allocation, Binding, Capacity, Growth, Kind, Object, Reply, Request};
use crate::storage::catalog::{RecordingCatalog, RecordingCatalogHandle, tests::test_dir};
use std::time::{Duration, Instant};

fn catalog(limit_bytes: Option<u64>) -> RecordingCatalog {
    let root = test_dir(&format!(
        "volume-growth-boundaries-{}",
        uuid::Uuid::new_v4()
    ));
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    for id in ["primary", "secondary"] {
        catalog
            .handle()
            .volume_location(Request::Bind(Binding {
                id: id.into(),
                generation: 1,
                root: root.join(id),
                filesystem: "shared-disk".into(),
                root_identity: format!("root-{id}"),
                writable: true,
                limit_bytes,
                minimum_free_bytes: 0,
            }))
            .unwrap();
    }
    catalog
}

fn capacity(handle: &RecordingCatalogHandle, volume: &str, available_bytes: u64) -> Capacity {
    Capacity {
        ledger_revision: handle.volume_ledger_revision().unwrap(),
        observed_at: Instant::now(),
        available_bytes,
        filesystem: "shared-disk".into(),
        root_identity: format!("root-{volume}"),
    }
}

fn reserve(handle: &RecordingCatalogHandle, operation: &str, volume: &str, bytes: u64) {
    handle
        .volume_location(Request::Reserve(Allocation {
            operation: operation.into(),
            object: Object {
                kind: Kind::Recording,
                id: operation.into(),
            },
            volume: volume.into(),
            generation: 1,
            relative_key: format!("{operation}.mp4"),
            bytes,
            capacity: capacity(handle, volume, i64::MAX as u64),
        }))
        .unwrap();
}

fn reject_without_mutation(handle: &RecordingCatalogHandle, growth: Growth) {
    let revision = handle.volume_ledger_revision().unwrap();
    let usage = handle.volume_location(Request::Usage).unwrap();
    assert!(handle.volume_location(Request::Grow(growth)).is_err());
    assert_eq!(handle.volume_ledger_revision().unwrap(), revision);
    assert_eq!(handle.volume_location(Request::Usage).unwrap(), usage);
}

#[test]
fn positive_growth_enforces_quota_with_ample_physical_space() {
    let catalog = catalog(Some(100));
    let handle = catalog.handle();
    reserve(&handle, "growing", "primary", 70);
    let mut growth = Growth {
        operation: "growing".into(),
        bytes: 101,
        capacity: capacity(&handle, "primary", 1_000),
    };
    reject_without_mutation(&handle, growth.clone());
    growth.bytes = 100;
    assert_eq!(
        handle.volume_location(Request::Grow(growth)).unwrap(),
        Reply::Reserved {
            operation: "growing".into(),
            bytes: 100,
        }
    );
    catalog.shutdown();
}

#[test]
fn positive_growth_counts_other_volumes_on_the_same_filesystem() {
    let catalog = catalog(None);
    let handle = catalog.handle();
    reserve(&handle, "growing", "primary", 40);
    reserve(&handle, "sibling", "secondary", 50);
    let mut growth = Growth {
        operation: "growing".into(),
        bytes: 51,
        capacity: capacity(&handle, "primary", 100),
    };
    reject_without_mutation(&handle, growth.clone());
    growth.bytes = 50;
    assert_eq!(
        handle.volume_location(Request::Grow(growth)).unwrap(),
        Reply::Reserved {
            operation: "growing".into(),
            bytes: 50,
        }
    );
    catalog.shutdown();
}

#[test]
fn positive_growth_rejects_volume_and_filesystem_accounting_overflow() {
    for sibling_volume in ["primary", "secondary"] {
        let catalog = catalog(None);
        let handle = catalog.handle();
        let ceiling = i64::MAX as u64;
        reserve(&handle, "growing", "primary", ceiling - 10);
        reserve(&handle, "sibling", sibling_volume, 5);
        reject_without_mutation(
            &handle,
            Growth {
                operation: "growing".into(),
                bytes: ceiling - 4,
                capacity: capacity(&handle, "primary", ceiling),
            },
        );
        reject_without_mutation(
            &handle,
            Growth {
                operation: "growing".into(),
                bytes: ceiling + 1,
                capacity: capacity(&handle, "primary", ceiling),
            },
        );
        catalog.shutdown();
    }
}

#[test]
fn positive_growth_rejects_expired_and_future_observations() {
    let catalog = catalog(None);
    let handle = catalog.handle();
    reserve(&handle, "growing", "primary", 70);
    for observed_at in [
        Instant::now() - Duration::from_secs(6),
        Instant::now() + Duration::from_secs(60),
    ] {
        let mut sample = capacity(&handle, "primary", 1_000);
        sample.observed_at = observed_at;
        reject_without_mutation(
            &handle,
            Growth {
                operation: "growing".into(),
                bytes: 80,
                capacity: sample,
            },
        );
    }
    assert_eq!(
        handle
            .volume_location(Request::Grow(Growth {
                operation: "growing".into(),
                bytes: 80,
                capacity: capacity(&handle, "primary", 1_000),
            }))
            .unwrap(),
        Reply::Reserved {
            operation: "growing".into(),
            bytes: 80
        }
    );
    catalog.shutdown();
}
