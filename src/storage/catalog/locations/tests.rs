use super::*;
use crate::storage::catalog::{RecordingCatalog, tests::test_dir};

#[test]
fn draining_binding_survives_restart_and_allows_only_existing_growth() {
    let root = test_dir("volume-draining-reopen");
    let path = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    let mut initial = binding(&root, "primary", "disk");
    handle
        .volume_location(Request::Bind(initial.clone()))
        .unwrap();
    let allocation = reserve(
        "existing",
        "primary",
        handle.volume_ledger_revision().unwrap(),
        10,
    );
    handle
        .volume_location(Request::Reserve(allocation))
        .unwrap();
    initial.draining = true;
    handle.volume_location(Request::Bind(initial)).unwrap();
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    let next = reserve(
        "new",
        "primary",
        handle.volume_ledger_revision().unwrap(),
        10,
    );
    assert!(
        handle
            .volume_location(Request::Reserve(next.clone()))
            .is_err()
    );
    assert_eq!(
        handle
            .volume_location(Request::Grow(Growth {
                operation: "existing".into(),
                bytes: 20,
                capacity: next.capacity,
            }))
            .unwrap(),
        Reply::Reserved {
            operation: "existing".into(),
            bytes: 20
        }
    );
    catalog.shutdown();
}

#[test]
fn old_bindings_gain_draining_state_without_changing_write_permissions() {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        let old_schema = include_str!("schema.sql")
            .split("CREATE TABLE IF NOT EXISTS storage_volume_allocations")
            .next()
            .unwrap()
            .lines()
            .filter(|line| !line.contains("draining INTEGER"))
            .collect::<Vec<_>>()
            .join("\n");
        connection.execute_batch(&old_schema).await.unwrap();
        connection.execute_batch("INSERT INTO storage_volume_bindings (id, generation, root, filesystem, root_identity, writable, minimum_free_bytes) VALUES ('legacy-active', 1, '/active', 'disk', 'active', 1, 0), ('legacy-archive', 1, '/archive', 'disk', 'archive', 0, 0)").await.unwrap();
        super::super::initialize_schema(&connection).await.unwrap();
        let mut rows = connection
            .query(
                "SELECT writable, draining FROM storage_volume_bindings ORDER BY id",
                (),
            )
            .await
            .unwrap();
        for writable in [1_i64, 0] {
            let row = rows.next().await.unwrap().unwrap();
            assert_eq!(row.get::<i64>(0).unwrap(), writable);
            assert_eq!(row.get::<i64>(1).unwrap(), 0);
        }
        drop(rows);
        connection
            .execute(
                "UPDATE storage_volume_bindings SET draining = 1 WHERE writable = 1",
                (),
            )
            .await
            .unwrap();
        super::super::initialize_schema(&connection).await.unwrap();
        let mut rows = connection
            .query(
                "SELECT draining FROM storage_volume_bindings WHERE id = 'legacy-active'",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1
        );
    });
}

#[test]
fn volume_ledger_growth_reserves_only_delta_and_survives_retries_and_restart() {
    let root = test_dir("volume-ledger-growth");
    let path = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    handle
        .volume_location(Request::Bind(binding(&root, "primary", "disk")))
        .unwrap();
    let allocation = reserve(
        "growing",
        "primary",
        handle.volume_ledger_revision().unwrap(),
        70,
    );
    handle
        .volume_location(Request::Reserve(allocation.clone()))
        .unwrap();
    let mut growth = Growth {
        operation: "growing".into(),
        bytes: 80,
        capacity: allocation.capacity.clone(),
    };
    assert!(
        handle
            .volume_location(Request::Grow(growth.clone()))
            .is_err()
    );
    growth.capacity.ledger_revision = handle.volume_ledger_revision().unwrap();
    let expected = Reply::Reserved {
        operation: "growing".into(),
        bytes: 80,
    };
    assert_growth_retries(&handle, growth.clone(), allocation, &expected);
    let revision = handle.volume_ledger_revision().unwrap();
    growth.bytes = 95;
    growth.capacity.ledger_revision = revision;
    assert!(
        handle
            .volume_location(Request::Grow(growth.clone()))
            .is_err()
    );
    assert_eq!(handle.volume_ledger_revision().unwrap(), revision);
    assert_growth_usage(&handle);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    growth.bytes = 80;
    assert_eq!(
        handle.volume_location(Request::Grow(growth)).unwrap(),
        expected
    );
    assert_growth_usage(&handle);
    catalog.shutdown();
}

fn assert_growth_retries(
    handle: &RecordingCatalogHandle,
    growth: Growth,
    allocation: Allocation,
    expected: &Reply,
) {
    for request in [
        Request::Grow(growth.clone()),
        Request::Grow(growth),
        Request::Reserve(allocation),
    ] {
        assert_eq!(&handle.volume_location(request).unwrap(), expected);
    }
}

fn assert_growth_usage(handle: &RecordingCatalogHandle) {
    assert_eq!(
        handle.volume_location(Request::Usage).unwrap(),
        Reply::Usage(vec![Usage {
            volume: "primary".into(),
            filesystem: "disk".into(),
            allocated_bytes: 80,
            reserved_bytes: 80
        }])
    );
}

fn binding(root: &std::path::Path, id: &str, filesystem: &str) -> Binding {
    Binding {
        id: id.to_owned(),
        generation: 1,
        root: root.join(id),
        filesystem: filesystem.to_owned(),
        root_identity: format!("root-{id}"),
        writable: true,
        draining: false,
        limit_bytes: Some(100),
        minimum_free_bytes: 10,
    }
}

fn reserve(id: &str, volume: &str, revision: u64, bytes: u64) -> Allocation {
    Allocation {
        operation: id.to_owned(),
        object: Object {
            kind: Kind::Recording,
            id: id.to_owned(),
        },
        volume: volume.to_owned(),
        generation: 1,
        relative_key: format!("camera/{id}.mp4"),
        bytes,
        capacity: Capacity {
            ledger_revision: revision,
            observed_at: Instant::now(),
            available_bytes: 100,
            filesystem: "disk".to_owned(),
            root_identity: format!("root-{volume}"),
        },
    }
}

#[test]
fn volume_ledger_reopen_preserves_binding_and_exact_retry() {
    let root = test_dir("volume-ledger-reopen");
    let path = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    handle
        .volume_location(Request::Bind(binding(&root, "primary", "disk")))
        .unwrap();
    let revision = handle.volume_ledger_revision().unwrap();
    let allocation = reserve("recording-one", "primary", revision, 70);
    let first = handle
        .volume_location(Request::Reserve(allocation.clone()))
        .unwrap();
    catalog.shutdown();
    let reopened = RecordingCatalog::open(&path).unwrap();
    let handle = reopened.handle();
    assert_eq!(
        first,
        handle
            .volume_location(Request::Reserve(allocation.clone()))
            .unwrap()
    );
    let mut changed = allocation;
    changed.bytes = 71;
    assert!(handle.volume_location(Request::Reserve(changed)).is_err());
    let mut rebound = binding(&root, "primary", "other-disk");
    rebound.generation = 2;
    assert!(handle.volume_location(Request::Bind(rebound)).is_err());
    reopened.shutdown();
}

#[test]
fn volume_ledger_rejects_reused_capacity_observation_and_shared_disk_overcommit() {
    let root = test_dir("volume-ledger-capacity");
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    let handle = catalog.handle();
    for id in ["primary", "secondary"] {
        handle
            .volume_location(Request::Bind(binding(&root, id, "disk")))
            .unwrap();
    }
    let revision = handle.volume_ledger_revision().unwrap();
    handle
        .volume_location(Request::Reserve(reserve("one", "primary", revision, 70)))
        .unwrap();
    assert!(
        handle
            .volume_location(Request::Reserve(reserve("two", "secondary", revision, 1)))
            .is_err()
    );
    let revision = handle.volume_ledger_revision().unwrap();
    assert!(
        handle
            .volume_location(Request::Reserve(reserve("two", "secondary", revision, 21)))
            .is_err()
    );
    handle
        .volume_location(Request::Reserve(reserve("two", "secondary", revision, 20)))
        .unwrap();
    catalog.shutdown();
}

#[test]
fn volume_ledger_rejects_unconfined_keys_and_expired_observations_atomically() {
    let root = test_dir("volume-ledger-invalid");
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    let handle = catalog.handle();
    handle
        .volume_location(Request::Bind(binding(&root, "primary", "disk")))
        .unwrap();
    let revision = handle.volume_ledger_revision().unwrap();
    for key in [
        "../file",
        "/absolute",
        "camera/../file",
        "camera\\file",
        "camera//file",
        "C:/file",
        "camera/CON",
        "camera/file.",
    ] {
        let mut request = reserve("invalid", "primary", revision, 1);
        request.relative_key = key.to_owned();
        assert!(
            handle.volume_location(Request::Reserve(request)).is_err(),
            "{key}"
        );
        assert_eq!(revision, handle.volume_ledger_revision().unwrap());
    }
    let mut request = reserve("expired", "primary", revision, 1);
    request.capacity.observed_at = Instant::now() - Duration::from_secs(6);
    assert!(handle.volume_location(Request::Reserve(request)).is_err());
    assert_eq!(revision, handle.volume_ledger_revision().unwrap());
    catalog.shutdown();
}

#[test]
fn volume_ledger_fences_policy_root_evidence_and_destination_aliases() {
    let root = test_dir("volume-ledger-fences");
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    let handle = catalog.handle();
    let initial = binding(&root, "primary", "disk");
    handle
        .volume_location(Request::Bind(initial.clone()))
        .unwrap();
    let revision = handle.volume_ledger_revision().unwrap();
    let mut request = reserve("one", "primary", revision, 10);
    request.capacity.filesystem = "wrong-disk".to_owned();
    assert!(
        handle
            .volume_location(Request::Reserve(request.clone()))
            .is_err()
    );
    request.capacity.filesystem = "disk".to_owned();
    request.capacity.root_identity = "other-root".to_owned();
    assert!(
        handle
            .volume_location(Request::Reserve(request.clone()))
            .is_err()
    );
    request.capacity.root_identity = "root-primary".to_owned();
    handle
        .volume_location(Request::Reserve(request.clone()))
        .unwrap();
    request.operation = "different-operation".to_owned();
    request.object.id = "different-object".to_owned();
    request.relative_key = "CAMERA/ONE.MP4".to_owned();
    request.capacity.ledger_revision = handle.volume_ledger_revision().unwrap();
    assert!(handle.volume_location(Request::Reserve(request)).is_err());
    let revision = handle.volume_ledger_revision().unwrap();
    let mut disabled = initial;
    disabled.writable = false;
    handle.volume_location(Request::Bind(disabled)).unwrap();
    assert!(
        handle
            .volume_location(Request::Reserve(reserve("two", "primary", revision, 10)))
            .is_err()
    );
    let revision = handle.volume_ledger_revision().unwrap();
    assert!(
        handle
            .volume_location(Request::Reserve(reserve("two", "primary", revision, 10)))
            .is_err()
    );
    catalog.shutdown();
}

#[test]
fn volume_ledger_reopen_invalidates_unused_capacity_ticket() {
    let root = test_dir("volume-ledger-epoch");
    let path = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    handle
        .volume_location(Request::Bind(binding(&root, "primary", "disk")))
        .unwrap();
    let allocation = reserve(
        "one",
        "primary",
        handle.volume_ledger_revision().unwrap(),
        10,
    );
    catalog.shutdown();
    let reopened = RecordingCatalog::open(&path).unwrap();
    assert!(
        reopened
            .handle()
            .volume_location(Request::Reserve(allocation))
            .is_err()
    );
    reopened.shutdown();
}

#[test]
fn volume_ledger_rejects_integer_overflow_without_spending_revision() {
    let root = test_dir("volume-ledger-overflow");
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    let handle = catalog.handle();
    let mut volume = binding(&root, "primary", "disk");
    volume.limit_bytes = None;
    volume.minimum_free_bytes = 0;
    handle.volume_location(Request::Bind(volume)).unwrap();
    let revision = handle.volume_ledger_revision().unwrap();
    let mut invalid = reserve("invalid", "primary", revision, 1);
    invalid.capacity.available_bytes = u64::MAX;
    assert!(handle.volume_location(Request::Reserve(invalid)).is_err());
    assert_eq!(revision, handle.volume_ledger_revision().unwrap());
    let maximum = u64::try_from(i64::MAX).unwrap();
    let mut first = reserve("first", "primary", revision, maximum);
    first.capacity.available_bytes = maximum;
    handle.volume_location(Request::Reserve(first)).unwrap();
    let revision = handle.volume_ledger_revision().unwrap();
    let mut second = reserve("second", "primary", revision, 1);
    second.capacity.available_bytes = maximum;
    assert!(handle.volume_location(Request::Reserve(second)).is_err());
    assert_eq!(revision, handle.volume_ledger_revision().unwrap());
    catalog.shutdown();
}

#[test]
fn volume_ledger_competing_callers_cannot_spend_one_probe_twice() {
    let root = test_dir("volume-ledger-concurrent");
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    let handle = catalog.handle();
    handle
        .volume_location(Request::Bind(binding(&root, "primary", "disk")))
        .unwrap();
    let revision = handle.volume_ledger_revision().unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for id in ["one", "two"] {
        let handle = handle.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            handle
                .volume_location(Request::Reserve(reserve(id, "primary", revision, 70)))
                .is_ok()
        }));
    }
    let successes = workers
        .into_iter()
        .filter_map(|worker| worker.join().unwrap().then_some(()))
        .count();
    assert_eq!(successes, 1);
    catalog.shutdown();
}

#[test]
fn volume_ledger_rollback_recovers_active_and_already_clean_transactions() {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        rollback(&connection).await.unwrap();
        connection.execute_batch("CREATE TABLE rollback_probe (value INTEGER); BEGIN IMMEDIATE; INSERT INTO rollback_probe VALUES (1)").await.unwrap();
        assert!(!connection.is_autocommit().unwrap());
        rollback(&connection).await.unwrap();
        assert!(connection.is_autocommit().unwrap());
        let mut rows = connection
            .query("SELECT COUNT(*) FROM rollback_probe", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
        rollback(&connection).await.unwrap();
    });
}

#[test]
fn volume_ledger_publication_pins_identity_and_does_not_reuse_spent_observation() {
    let root = test_dir("volume-ledger-publication");
    let path = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    handle
        .volume_location(Request::Bind(binding(&root, "primary", "disk")))
        .unwrap();
    let allocation = reserve(
        "one",
        "primary",
        handle.volume_ledger_revision().unwrap(),
        70,
    );
    handle
        .volume_location(Request::Reserve(allocation.clone()))
        .unwrap();
    assert_eq!(
        handle
            .volume_location(Request::Lookup(allocation.object.clone()))
            .unwrap(),
        Reply::Location(None)
    );
    let probe_revision = handle.volume_ledger_revision().unwrap();
    let publication = Publication {
        operation: "one".to_owned(),
        bytes: 60,
        file_identity: "file-one".to_owned(),
        digest: [42; 32],
    };
    assert!(
        handle
            .volume_location(Request::Publish(publication.clone()))
            .is_err()
    );
    seed_recording(&handle, &root, "one");
    let result = handle
        .volume_location(Request::Publish(publication.clone()))
        .unwrap();
    let Reply::Location(Some(location)) = &result else {
        panic!("published location is missing");
    };
    assert_eq!(location.object, allocation.object);
    assert_eq!(location.bytes, 60);
    assert_eq!(location.digest, [42; 32]);
    assert_eq!(location.revision, 1);
    assert_published_recording_fences(&handle, &root);
    assert_post_publication_capacity(&handle, probe_revision);
    let mut conflicting = publication.clone();
    conflicting.digest = [43; 32];
    assert!(
        handle
            .volume_location(Request::Publish(conflicting))
            .is_err()
    );
    catalog.shutdown();
    assert_publication_survives_reopen(&path, publication, allocation.object, result);
}

fn assert_publication_survives_reopen(
    path: &std::path::Path,
    publication: Publication,
    object: Object,
    expected: Reply,
) {
    let reopened = RecordingCatalog::open(path).unwrap();
    assert_eq!(
        reopened
            .handle()
            .volume_location(Request::Publish(publication))
            .unwrap(),
        expected
    );
    assert_eq!(
        reopened
            .handle()
            .volume_location(Request::Lookup(object))
            .unwrap(),
        expected
    );
    reopened.shutdown();
}

#[test]
fn volume_ledger_maintenance_conflicts_are_symmetric() {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        super::super::initialize_schema(&connection).await.unwrap();
        let root = test_dir("volume-ledger-maintenance-fence");
        execute(
            &connection,
            Request::Bind(binding(&root, "primary", "disk")),
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        let revision = revision(&connection).await.unwrap();
        let first = reserve("one", "primary", revision, 10);
        super::reserve(&connection, &first).await.unwrap();
        let insert_claim = "INSERT INTO recording_maintenance_claims (job_id, ordinal, recording_id, token, path, file_identity, file_bytes, active) VALUES ('job', ?1, ?2, ?2, ?2, ?3, 10, 1)";
        assert!(
            connection
                .execute(insert_claim, turso::params![1_i64, "one", vec![0_u8; 32]])
                .await
                .is_err()
        );
        connection
            .execute(insert_claim, turso::params![2_i64, "two", vec![0_u8; 32]])
            .await
            .unwrap();
        let revision = super::revision(&connection).await.unwrap();
        assert!(
            super::reserve(&connection, &reserve("two", "primary", revision, 10))
                .await
                .is_err()
        );
        assert_eq!(super::revision(&connection).await.unwrap(), revision);
    });
}

fn assert_post_publication_capacity(handle: &RecordingCatalogHandle, probe_revision: u64) {
    assert!(
        handle
            .volume_location(Request::Reserve(reserve(
                "two",
                "primary",
                probe_revision,
                1
            )))
            .is_err()
    );
    let mut second = reserve(
        "two",
        "primary",
        handle.volume_ledger_revision().unwrap(),
        31,
    );
    // The new filesystem sample includes the written bytes; quota still includes ownership.
    second.capacity.available_bytes = 40;
    assert!(
        handle
            .volume_location(Request::Reserve(second.clone()))
            .is_err()
    );
    second.bytes = 30;
    handle.volume_location(Request::Reserve(second)).unwrap();
}

fn seed_recording(handle: &RecordingCatalogHandle, root: &std::path::Path, id: &str) {
    let path = root
        .join("primary")
        .join("camera")
        .join(format!("{id}.mp4"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, [42; 60]).unwrap();
    handle
        .upsert_recording(crate::storage::catalog::CatalogRecording {
            id: id.to_owned(),
            stream_id: "camera/main".to_owned(),
            source_id: Some("camera".to_owned()),
            logical_stream_id: Some("main".to_owned()),
            started_at_ms: 1,
            ended_at_ms: Some(2),
            path: path.to_string_lossy().into_owned(),
            init_offset: 0,
            init_len: 1,
            finalized: true,
        })
        .unwrap();
}

fn assert_published_recording_fences(handle: &RecordingCatalogHandle, root: &std::path::Path) {
    assert!(
        handle
            .update_recording_path("one", &root.join("other.mp4"), true)
            .is_err()
    );
    assert!(handle.claim_cleanup_candidate().unwrap().is_none());
    let path = root.join("primary/camera/one.mp4");
    std::fs::write(&path, [43; 61]).unwrap();
    assert!(handle.update_recording_path("one", &path, true).is_err());
}

#[test]
fn volume_ledger_path_conflicts_do_not_depend_on_recording_identity() {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        super::super::initialize_schema(&connection).await.unwrap();
        let root = test_dir("volume-ledger-path-fence");
        execute(
            &connection,
            Request::Bind(binding(&root, "primary", "disk")),
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        let revision = super::revision(&connection).await.unwrap();
        let first = reserve("one", "primary", revision, 10);
        super::reserve(&connection, &first).await.unwrap();
        let insert_claim = "INSERT INTO recording_maintenance_claims (job_id, ordinal, recording_id, token, path, file_identity, file_bytes, active) VALUES ('job', ?1, ?2, ?2, ?3, ?4, 10, 1)";
        let first_path = root
            .join("primary/camera/one.mp4")
            .to_string_lossy()
            .into_owned();
        assert!(
            connection
                .execute(
                    insert_claim,
                    turso::params![1_i64, "different-id", first_path, vec![0_u8; 32]]
                )
                .await
                .is_err()
        );
        let second_path = root
            .join("primary/camera/two.mp4")
            .to_string_lossy()
            .into_owned();
        connection
            .execute(
                insert_claim,
                turso::params![2_i64, "another-id", second_path, vec![0_u8; 32]],
            )
            .await
            .unwrap();
        let revision = super::revision(&connection).await.unwrap();
        assert!(
            super::reserve(&connection, &reserve("two", "primary", revision, 10))
                .await
                .is_err()
        );
        assert_eq!(super::revision(&connection).await.unwrap(), revision);
    });
}

#[test]
fn volume_ledger_cleanup_path_conflicts_are_symmetric_and_preserve_rows() {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        super::super::initialize_schema(&connection).await.unwrap();
        let root = test_dir("volume-ledger-cleanup-fence");
        execute(
            &connection,
            Request::Bind(binding(&root, "primary", "disk")),
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        let insert_recording = "INSERT INTO recording_files (id, stream_id, started_at_ms, path, init_offset, init_len, finalized, file_bytes, cleanup_pending) VALUES (?1, 'camera', 1, ?2, 0, 1, 1, 10, ?3)";
        let first_path = root
            .join("primary/camera/one.mp4")
            .to_string_lossy()
            .into_owned();
        connection
            .execute(
                insert_recording,
                turso::params!["old-one", first_path, 0_i64],
            )
            .await
            .unwrap();
        let revision = super::revision(&connection).await.unwrap();
        super::reserve(&connection, &reserve("one", "primary", revision, 10))
            .await
            .unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE recording_files SET cleanup_pending = 1 WHERE id = 'old-one'",
                    ()
                )
                .await
                .is_err()
        );
        assert!(
            connection
                .execute("DELETE FROM recording_files WHERE id = 'old-one'", ())
                .await
                .is_err()
        );
        let second_path = root
            .join("primary/camera/two.mp4")
            .to_string_lossy()
            .into_owned();
        connection
            .execute(
                insert_recording,
                turso::params!["old-two", second_path, 1_i64],
            )
            .await
            .unwrap();
        let revision = super::revision(&connection).await.unwrap();
        assert!(
            super::reserve(&connection, &reserve("two", "primary", revision, 10))
                .await
                .is_err()
        );
        assert_eq!(super::revision(&connection).await.unwrap(), revision);
        let query = "SELECT cleanup_pending FROM recording_files WHERE id = 'old-one'";
        let mut rows = connection.query(query, ()).await.unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
    });
}

#[test]
fn volume_ledger_publication_rejects_wrong_path_and_unfinalized_recordings() {
    let root = test_dir("volume-ledger-publication-state");
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    let handle = catalog.handle();
    handle
        .volume_location(Request::Bind(binding(&root, "primary", "disk")))
        .unwrap();
    handle
        .volume_location(Request::Reserve(reserve(
            "one",
            "primary",
            handle.volume_ledger_revision().unwrap(),
            70,
        )))
        .unwrap();
    seed_recording(&handle, &root, "one");
    let publication = Publication {
        operation: "one".to_owned(),
        bytes: 60,
        file_identity: "file".to_owned(),
        digest: [42; 32],
    };
    let revision = handle.volume_ledger_revision().unwrap();
    handle
        .update_recording_path("one", &root.join("wrong.mp4"), true)
        .unwrap();
    assert!(
        handle
            .volume_location(Request::Publish(publication.clone()))
            .is_err()
    );
    let path = root.join("primary/camera/one.mp4");
    handle.update_recording_path("one", &path, false).unwrap();
    assert!(
        handle
            .volume_location(Request::Publish(publication.clone()))
            .is_err()
    );
    assert_eq!(handle.volume_ledger_revision().unwrap(), revision);
    handle.update_recording_path("one", &path, true).unwrap();
    assert!(matches!(
        handle
            .volume_location(Request::Publish(publication))
            .unwrap(),
        Reply::Location(Some(_))
    ));
    catalog.shutdown();
}

#[test]
fn volume_ledger_pending_cleanup_cannot_change_into_a_reserved_path() {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await
            .unwrap();
        let connection = database.connect().unwrap();
        super::super::initialize_schema(&connection).await.unwrap();
        let root = test_dir("volume-ledger-cleanup-path-change");
        execute(
            &connection,
            Request::Bind(binding(&root, "primary", "disk")),
            Instant::now() + BUSY_TIMEOUT,
        )
        .await
        .unwrap();
        let insert = "INSERT INTO recording_files (id, stream_id, started_at_ms, path, init_offset, init_len, finalized, file_bytes, cleanup_pending) VALUES ('old', 'camera', 1, 'old.mp4', 0, 1, 1, 10, 1)";
        connection.execute(insert, ()).await.unwrap();
        let revision = super::revision(&connection).await.unwrap();
        super::reserve(&connection, &reserve("one", "primary", revision, 10))
            .await
            .unwrap();
        let revision = super::revision(&connection).await.unwrap();
        let destination = root
            .join("primary/camera/one.mp4")
            .to_string_lossy()
            .into_owned();
        assert!(
            connection
                .execute(
                    "UPDATE recording_files SET path = ?1 WHERE id = 'old'",
                    [destination]
                )
                .await
                .is_err()
        );
        assert_eq!(super::revision(&connection).await.unwrap(), revision);
        let mut rows = connection
            .query(
                "SELECT path, cleanup_pending FROM recording_files WHERE id = 'old'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<String>(0).unwrap(), "old.mp4");
        assert_eq!(row.get::<i64>(1).unwrap(), 1);
    });
}

#[test]
fn volume_ledger_publication_cannot_exceed_reserved_capacity() {
    let root = test_dir("volume-ledger-publication-overflow");
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    let handle = catalog.handle();
    handle
        .volume_location(Request::Bind(binding(&root, "primary", "disk")))
        .unwrap();
    handle
        .volume_location(Request::Reserve(reserve(
            "one",
            "primary",
            handle.volume_ledger_revision().unwrap(),
            10,
        )))
        .unwrap();
    let revision = handle.volume_ledger_revision().unwrap();
    assert!(
        handle
            .volume_location(Request::Publish(Publication {
                operation: "one".to_owned(),
                bytes: 11,
                file_identity: "file-one".to_owned(),
                digest: [42; 32]
            }))
            .is_err()
    );
    assert_eq!(handle.volume_ledger_revision().unwrap(), revision);
    assert_eq!(
        handle
            .volume_location(Request::Lookup(Object {
                kind: Kind::Recording,
                id: "one".to_owned()
            }))
            .unwrap(),
        Reply::Location(None)
    );
    catalog.shutdown();
}
