use super::*;
use crate::storage::catalog::{RecordingCatalog, tests::test_dir};

#[test]
fn legacy_pending_allocation_migrates_with_its_entire_reservation_outstanding() {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:").build().await.unwrap();
        let connection = database.connect().unwrap();
        connection.execute_batch("CREATE TABLE storage_volume_allocations(operation TEXT PRIMARY KEY, bytes INTEGER NOT NULL); INSERT INTO storage_volume_allocations VALUES ('pending', 90);").await.unwrap();
        materialization::migrate(&connection).await.unwrap();
        materialization::migrate(&connection).await.unwrap();
        let mut rows = connection.query("SELECT bytes, materialized_bytes FROM storage_volume_allocations WHERE operation = 'pending'", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 90);
        assert_eq!(row.get::<i64>(1).unwrap(), 0);
        assert!(
            connection
                .execute(
                    "UPDATE storage_volume_allocations SET materialized_bytes = 91",
                    ()
                )
                .await
                .is_err()
        );
    });
}

#[test]
fn materialized_bytes_release_only_physical_reservations_and_survive_restart() {
    let root = test_dir("volume-materialization");
    let path = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    reserve_fixture(&handle, &root);
    let checkpoint = Materialization {
        operation: "writer".into(),
        bytes: 70,
        file_identity: "file".into(),
    };
    handle
        .volume_location(Request::Materialize(checkpoint.clone()))
        .unwrap();
    let revision = handle.volume_ledger_revision().unwrap();
    handle
        .volume_location(Request::Materialize(checkpoint.clone()))
        .unwrap();
    assert_eq!(handle.volume_ledger_revision().unwrap(), revision);
    assert_checkpoint_usage(&handle, 80, 10);
    for (bytes, identity) in [(69, "file"), (81, "file"), (70, "replacement")] {
        assert!(
            handle
                .volume_location(Request::Materialize(Materialization {
                    bytes,
                    file_identity: identity.into(),
                    ..checkpoint.clone()
                }))
                .is_err()
        );
        assert_eq!(handle.volume_ledger_revision().unwrap(), revision);
    }
    handle
        .volume_location(Request::Grow(Growth {
            operation: "writer".into(),
            bytes: 90,
            capacity: Capacity {
                ledger_revision: revision,
                observed_at: Instant::now(),
                available_bytes: 30,
                filesystem: "disk".into(),
                root_identity: "root".into(),
            },
        }))
        .unwrap();
    assert_checkpoint_usage(&handle, 90, 20);
    catalog.shutdown();
    let reopened = RecordingCatalog::open(&path).unwrap();
    assert_checkpoint_usage(&reopened.handle(), 90, 20);
    reopened
        .handle()
        .volume_location(Request::Materialize(checkpoint))
        .unwrap();
    assert_checkpoint_usage(&reopened.handle(), 90, 20);
    reopened.shutdown();
}

fn assert_checkpoint_usage(handle: &RecordingCatalogHandle, allocated: u64, reserved: u64) {
    assert_eq!(
        handle.volume_location(Request::Usage).unwrap(),
        Reply::Usage(vec![Usage {
            volume: "primary".into(),
            filesystem: "disk".into(),
            allocated_bytes: allocated,
            reserved_bytes: reserved,
        }])
    );
}

fn reserve_fixture(handle: &RecordingCatalogHandle, root: &std::path::Path) {
    handle
        .volume_location(Request::Bind(Binding {
            id: "primary".into(),
            generation: 1,
            root: root.join("primary"),
            filesystem: "disk".into(),
            root_identity: "root".into(),
            writable: true,
            draining: false,
            limit_bytes: Some(100),
            minimum_free_bytes: 10,
        }))
        .unwrap();
    let capacity = Capacity {
        ledger_revision: handle.volume_ledger_revision().unwrap(),
        observed_at: Instant::now(),
        available_bytes: 100,
        filesystem: "disk".into(),
        root_identity: "root".into(),
    };
    handle
        .volume_location(Request::Reserve(Allocation {
            operation: "writer".into(),
            object: Object {
                kind: Kind::Export,
                id: "export".into(),
            },
            volume: "primary".into(),
            generation: 1,
            relative_key: "export.mp4".into(),
            bytes: 80,
            capacity,
        }))
        .unwrap();
}
