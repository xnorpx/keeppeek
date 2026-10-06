use super::*;
use std::sync::atomic::AtomicUsize;
use tracing_subscriber::{Layer, layer::Context, prelude::*};

struct Updates(Arc<AtomicUsize>);

impl<S: tracing::Subscriber> Layer<S> for Updates {
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: Context<'_, S>,
    ) {
        if attributes.metadata().name() == "update_recording_path" {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[test]
fn startup_skips_complete_finalization_but_repairs_missing_timestamps() {
    let root = tests::test_dir("startup-finalization-refresh");
    let path = root.join("recording.mp4");
    std::fs::write(&path, [1, 2, 3]).unwrap();
    let database_path = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&database_path).unwrap();
    let handle = catalog.handle();
    handle
        .upsert_recording(CatalogRecording {
            id: "recording-1".into(),
            stream_id: "front/main".into(),
            source_id: Some("front".into()),
            logical_stream_id: Some("main".into()),
            started_at_ms: 1_000,
            ended_at_ms: None,
            path: path.to_string_lossy().into_owned(),
            init_offset: 0,
            init_len: 0,
            finalized: false,
        })
        .unwrap();
    handle
        .insert_fragment_with_keyframe(tests::test_fragment(), tests::test_keyframe())
        .unwrap();
    handle
        .update_recording_path("recording-1", &path, true)
        .unwrap();
    let database =
        pollster::block_on(turso::Builder::new_local(database_path.to_str().unwrap()).build())
            .unwrap();
    let connection = database.connect().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(Updates(count.clone()));
    tracing::subscriber::with_default(subscriber, || {
        let recordings = pollster::block_on(prepare_legacy_backfill(&connection)).unwrap();
        backfill_legacy_recordings(handle.clone(), recordings, Arc::new(AtomicBool::new(false)));
        assert_eq!(
            count.load(Ordering::Relaxed),
            0,
            "complete rows must not queue another actor transaction"
        );
        pollster::block_on(connection.execute_batch(
            "UPDATE recording_files SET finalized_at_ms = NULL, ended_at_ms = NULL",
        ))
        .unwrap();
        let recordings = pollster::block_on(prepare_legacy_backfill(&connection)).unwrap();
        backfill_legacy_recordings(handle.clone(), recordings, Arc::new(AtomicBool::new(false)));
        assert_eq!(count.load(Ordering::Relaxed), 1);
    });
    assert_recovered_metadata(&connection);
    drop(connection);
    drop(database);
    drop(handle);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

fn assert_recovered_metadata(connection: &turso::Connection) {
    pollster::block_on(async {
        let mut rows = connection.query("SELECT file_bytes, file_identity, finalized_at_ms, ended_at_ms FROM recording_files", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 3);
        assert!(row.get::<Option<String>>(1).unwrap().is_some());
        assert!(row.get::<Option<i64>>(2).unwrap().is_some());
        assert_eq!(row.get::<i64>(3).unwrap(), 3_000);
        assert_eq!(
            tests::query_count(connection, "SELECT COUNT(*) FROM recording_coverage_ranges").await,
            1
        );
    });
}
