use crate::storage::catalog::{CatalogRecording, RecordingCatalog};
use crate::storage::retention::settings::Settings;

struct Directory(std::path::PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("retention-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn recording(id: &str, root: &std::path::Path) -> CatalogRecording {
    let path = root.join(format!("{id}.mp4"));
    std::fs::write(&path, b"media").unwrap();
    CatalogRecording {
        id: id.into(),
        stream_id: "front/main".into(),
        source_id: Some("front".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: 1000,
        ended_at_ms: Some(2000),
        path: path.to_string_lossy().into_owned(),
        init_offset: 0,
        init_len: 0,
        finalized: true,
    }
}

fn drain(catalog: &RecordingCatalog) -> u32 {
    let mut quarantined = 0;
    for _ in 0..128 {
        let progress = catalog.handle().reconcile_retention_runtime(4).unwrap();
        quarantined += progress.quarantined;
        if !progress.pending {
            return quarantined;
        }
    }
    panic!("bounded fixture failed to finish runtime reconciliation");
}

fn assert_current_retention_file(
    handle: &crate::storage::catalog::RecordingCatalogHandle,
    id: &str,
) {
    let owner = handle.retention.upgrade().unwrap();
    let connection = owner.connection.lock().unwrap();
    let mut rows = pollster::block_on(connection.query(
        "SELECT r.retention_pending,r.retention_generation,r.retention_error,
                s.generation,s.request_pending FROM recording_files r
         CROSS JOIN recording_retention_runtime s WHERE r.id=?1 AND s.singleton=1",
        [id],
    ))
    .unwrap();
    let row = pollster::block_on(rows.next()).unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 0);
    assert_eq!(row.get::<i64>(1).unwrap(), row.get::<i64>(3).unwrap());
    assert_eq!(row.get::<Option<String>>(2).unwrap(), None);
    assert_eq!(row.get::<i64>(4).unwrap(), 0);
}

fn seed_interrupted_archive(handle: &crate::storage::catalog::RecordingCatalogHandle) {
    let owner = handle.retention.upgrade().unwrap();
    let connection = owner.connection.lock().unwrap();
    pollster::block_on(connection.execute_batch(
        "INSERT INTO archive_bindings VALUES('cold','/archive','fs','root');
        INSERT INTO archive_replicas(job_id,recording_id,kind,target_id,object_id,source_path,
            source_identity,bytes,digest,source_root,source_filesystem,source_root_identity,
            source_relative_key,source_operation,state,local_evicted,action)
        VALUES('pause','a','recording','cold','6c0a2dda-bd3f-4e33-8224-77c69170820c',
            '/source/a.mp4','identity',4,zeroblob(32),'/source','fs','root','a.mp4','source',
            'ready',2,'evict')",
    ))
    .unwrap();
}

#[test]
fn policy_change_waits_for_interrupted_archive_eviction_without_quarantine() {
    let root = Directory::new();
    let path = root.path().join("catalog.db");
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    handle
        .upsert_recording(recording("a", root.path()))
        .unwrap();
    let original: Settings = toml::from_str("[default]\ncontinuous_days=10.0").unwrap();
    handle.request_retention_settings(Some(&original)).unwrap();
    drain(&catalog);
    let before = handle.retention_decision("a").unwrap().unwrap();
    seed_interrupted_archive(&handle);
    handle
        .upsert_recording(recording("b", root.path()))
        .unwrap();
    let shorter: Settings = toml::from_str("[default]\ncontinuous_days=1.0").unwrap();
    handle.request_retention_settings(Some(&shorter)).unwrap();
    let owner = handle.retention.upgrade().unwrap();
    let connection = owner.connection.lock().unwrap();
    pollster::block_on(connection.execute(
        "UPDATE recording_files SET retention_pending=1 WHERE id='a'",
        (),
    ))
    .unwrap();
    drop(connection);
    drop(owner);
    let mut evaluated = false;
    for _ in 0..3 {
        let progress = handle.reconcile_retention_runtime(2).unwrap();
        assert!(progress.pending && progress.activation_pending);
        assert_eq!(progress.quarantined, 0);
        evaluated |= progress.evaluated > 0;
    }
    assert!(evaluated);
    assert!(handle.retention_decision("b").unwrap().is_some());
    assert_eq!(
        handle.retention_decision("a").unwrap(),
        Some(before.clone())
    );
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    let handle = catalog.handle();
    assert!(handle.reconcile_retention_runtime(2).unwrap().pending);
    let owner = handle.retention.upgrade().unwrap();
    let connection = owner.connection.lock().unwrap();
    pollster::block_on(connection.execute(
        "UPDATE archive_replicas SET local_evicted=1,action='' WHERE job_id='pause'",
        (),
    ))
    .unwrap();
    drop(connection);
    drop(owner);
    assert_eq!(drain(&catalog), 0);
    assert_current_retention_file(&handle, "a");
    let after = handle.retention_decision("a").unwrap().unwrap();
    assert!(after.policy_revision > before.policy_revision);
    assert_eq!(after.deadline_ms, before.deadline_ms);
    catalog.shutdown();
}

#[test]
fn activation_fences_cleanup_and_survives_restart_and_shorter_settings() {
    let root = Directory::new();
    let catalog = RecordingCatalog::open(&root.path().join("catalog.db")).unwrap();
    let handle = catalog.handle();
    handle
        .upsert_recording(recording("a", root.path()))
        .unwrap();
    let settings: Settings = toml::from_str("[default]\ncontinuous_days=36500.0").unwrap();
    handle.request_retention_settings(Some(&settings)).unwrap();
    assert!(handle.claim_cleanup_candidate().is_err());
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.path().join("catalog.db")).unwrap();
    drain(&catalog);
    let before = catalog.handle().retention_decision("a").unwrap().unwrap();
    assert_eq!(before.deadline_ms, Some(3_153_600_002_000));
    assert!(
        catalog
            .handle()
            .claim_cleanup_candidate()
            .unwrap()
            .is_none()
    );
    let shorter: Settings = toml::from_str("[default]\ncontinuous_days=0.0").unwrap();
    catalog
        .handle()
        .request_retention_settings(Some(&shorter))
        .unwrap();
    drain(&catalog);
    assert_eq!(
        catalog
            .handle()
            .retention_decision("a")
            .unwrap()
            .unwrap()
            .deadline_ms,
        before.deadline_ms
    );
    catalog.handle().request_retention_settings(None).unwrap();
    drain(&catalog);
    assert!(
        catalog
            .handle()
            .claim_cleanup_candidate()
            .unwrap()
            .is_none()
    );
    assert_eq!(std::fs::read(root.path().join("a.mp4")).unwrap(), b"media");
    catalog.shutdown();
}

#[test]
fn finalization_behind_the_backfill_cursor_remains_fenced_until_evaluated() {
    let root = Directory::new();
    let catalog = RecordingCatalog::open(&root.path().join("catalog.db")).unwrap();
    let handle = catalog.handle();
    for id in ["b", "c", "d"] {
        handle.upsert_recording(recording(id, root.path())).unwrap();
    }
    let settings: Settings = toml::from_str("[default]\ncontinuous_days=36500.0").unwrap();
    handle.request_retention_settings(Some(&settings)).unwrap();
    assert!(handle.reconcile_retention_runtime(2).unwrap().pending);
    handle
        .upsert_recording(recording("a", root.path()))
        .unwrap();
    drain(&catalog);
    assert!(handle.retention_decision("a").unwrap().is_some());
    assert!(handle.claim_cleanup_candidate().unwrap().is_none());
    catalog.shutdown();
}

#[test]
fn accepted_settings_transition_cannot_be_overwritten_before_activation() {
    let root = Directory::new();
    let catalog = RecordingCatalog::open(&root.path().join("catalog.db")).unwrap();
    let handle = catalog.handle();
    handle
        .upsert_recording(recording("a", root.path()))
        .unwrap();
    let long: Settings = toml::from_str("[default]\ncontinuous_days=30.0").unwrap();
    let short: Settings = toml::from_str("[default]\ncontinuous_days=1.0").unwrap();
    assert!(handle.request_retention_settings(Some(&long)).unwrap());
    assert!(!handle.request_retention_settings(Some(&short)).unwrap());
    drain(&catalog);
    assert_eq!(
        handle.retention_decision("a").unwrap().unwrap().deadline_ms,
        Some(2_592_002_000)
    );
    assert!(handle.request_retention_settings(Some(&short)).unwrap());
    drain(&catalog);
    assert_eq!(
        handle.retention_decision("a").unwrap().unwrap().deadline_ms,
        Some(2_592_002_000)
    );
    catalog.shutdown();
}

#[test]
fn expiry_cursor_skips_protection_and_revisits_released_recordings() {
    let root = Directory::new();
    let catalog = RecordingCatalog::open(&root.path().join("catalog.db")).unwrap();
    let handle = catalog.handle();
    for id in ["a", "b", "c"] {
        handle.upsert_recording(recording(id, root.path())).unwrap();
    }
    handle.set_recording_protected("b", true).unwrap();
    let settings: Settings = toml::from_str("[default]\ncontinuous_days=0.0").unwrap();
    handle.request_retention_settings(Some(&settings)).unwrap();
    drain(&catalog);
    assert_eq!(handle.expired_retention_candidates(1).unwrap(), ["a"]);
    assert_eq!(handle.expired_retention_candidates(1).unwrap(), ["c"]);
    assert!(handle.expired_retention_candidates(1).unwrap().is_empty());
    handle.set_recording_protected("b", false).unwrap();
    assert_eq!(
        handle.expired_retention_candidates(8).unwrap(),
        ["a", "b", "c"]
    );
    catalog.shutdown();
}

fn publish_event(handle: &crate::storage::catalog::RecordingCatalogHandle, id: &str) {
    let owner = handle.retention.upgrade().unwrap();
    let connection = owner.connection.lock().unwrap();
    pollster::block_on(connection.execute(
        "INSERT INTO recording_events(id,camera_id,source,kind,start_time_ms,end_time_ms)
         VALUES(?1,'front','keeppeek','person',1000,2000)",
        [id],
    ))
    .unwrap();
}

#[test]
fn policy_change_drains_prior_events_without_waiting_for_live_events() {
    let root = Directory::new();
    let catalog = RecordingCatalog::open(&root.path().join("catalog.db")).unwrap();
    let handle = catalog.handle();
    handle
        .upsert_recording(recording("a", root.path()))
        .unwrap();
    let long: Settings = toml::from_str("[default.events]\nperson=30.0").unwrap();
    let short: Settings = toml::from_str("[default.events]\nperson=1.0").unwrap();
    handle.request_retention_settings(Some(&long)).unwrap();
    drain(&catalog);
    publish_event(&handle, "prior");
    handle.request_retention_settings(Some(&short)).unwrap();
    let mut activated = false;
    for index in 0..64 {
        publish_event(&handle, &format!("live-{index}"));
        let progress = handle.reconcile_retention_runtime(1).unwrap();
        if !progress.activation_pending {
            activated = true;
            break;
        }
    }
    assert!(activated, "live events prevented the accepted transition");
    drain(&catalog);
    assert_eq!(
        handle.retention_decision("a").unwrap().unwrap().deadline_ms,
        Some(2_592_002_000)
    );
    catalog.shutdown();
}

#[test]
fn expiry_deadline_cursor_uses_native_index_without_archive_scan() {
    let root = Directory::new();
    let catalog = RecordingCatalog::open(&root.path().join("catalog.db")).unwrap();
    let handle = catalog.handle();
    let owner = handle.retention.upgrade().unwrap();
    {
        let connection = owner.connection.lock().unwrap();
        let mut rows = pollster::block_on(connection.query(
            "EXPLAIN QUERY PLAN SELECT recording_id,expiry_at_ms
             FROM recording_retention_decisions INDEXED BY recording_retention_runtime_deadline
             WHERE runtime_generation=1 AND expiry_eligible=1 AND expiry_at_ms>0
             AND expiry_at_ms<=10000 ORDER BY expiry_at_ms,recording_id LIMIT 8",
            (),
        ))
        .unwrap();
        let mut indexed = false;
        for _ in 0..16 {
            let Some(row) = pollster::block_on(rows.next()).unwrap() else {
                break;
            };
            let detail: String = row.get(3).unwrap();
            assert!(
                !detail.contains("SORT") && !detail.contains("SCAN"),
                "{detail}"
            );
            indexed |= detail.contains("recording_retention_runtime_deadline");
        }
        assert!(indexed);
    }
    drop(owner);
    catalog.shutdown();
}

#[test]
fn oversized_event_evidence_quarantines_one_file_and_recovers_after_repair() {
    let root = Directory::new();
    let catalog = RecordingCatalog::open(&root.path().join("catalog.db")).unwrap();
    let handle = catalog.handle();
    handle
        .upsert_recording(recording("crowded", root.path()))
        .unwrap();
    let mut other = recording("other", root.path());
    other.source_id = Some("other".into());
    other.stream_id = "other/main".into();
    handle.upsert_recording(other).unwrap();
    for index in 0..257 {
        publish_event(&handle, &format!("event-{index}"));
    }
    let settings: Settings = toml::from_str("[default.events]\nperson=1.0").unwrap();
    handle.request_retention_settings(Some(&settings)).unwrap();
    drain(&catalog);
    assert!(handle.retention_decision("crowded").unwrap().is_none());
    assert!(handle.retention_decision("other").unwrap().is_some());
    assert_eq!(handle.expired_retention_candidates(8).unwrap(), ["other"]);
    let owner = handle.retention.upgrade().unwrap();
    {
        let connection = owner.connection.lock().unwrap();
        pollster::block_on(
            connection.execute("DELETE FROM recording_events WHERE id='event-0'", ()),
        )
        .unwrap();
    }
    drop(owner);
    drain(&catalog);
    assert_eq!(
        handle
            .retention_decision("crowded")
            .unwrap()
            .unwrap()
            .deadline_ms,
        Some(86_402_000)
    );
    assert_eq!(
        std::fs::read(root.path().join("crowded.mp4")).unwrap(),
        b"media"
    );
    catalog.shutdown();
}

#[test]
fn retention_event_hooks_follow_active_policy_across_restart_and_disable() {
    let root = Directory::new();
    let path = root.path().join("catalog.db");
    let catalog = RecordingCatalog::open(&path).unwrap();
    assert_eq!(event_hook_count(&catalog), 0);
    let settings: Settings = toml::from_str("[default]\nmotion_days=7.0").unwrap();
    catalog
        .handle()
        .request_retention_settings(Some(&settings))
        .unwrap();
    drain(&catalog);
    assert_eq!(event_hook_count(&catalog), 3);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    assert_eq!(event_hook_count(&catalog), 3);
    catalog.handle().request_retention_settings(None).unwrap();
    drain(&catalog);
    assert_eq!(event_hook_count(&catalog), 0);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path).unwrap();
    assert_eq!(event_hook_count(&catalog), 0);
    catalog.shutdown();
}

fn event_hook_count(catalog: &RecordingCatalog) -> i64 {
    let handle = catalog.handle();
    let owner = handle.retention.upgrade().unwrap();
    let connection = owner.connection.lock().unwrap();
    let mut rows = pollster::block_on(connection.query(
        "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name IN
        ('recording_retention_runtime_event_insert','recording_retention_runtime_event_update',
        'recording_retention_runtime_event_delete')",
        (),
    ))
    .unwrap();
    pollster::block_on(rows.next())
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap()
}
