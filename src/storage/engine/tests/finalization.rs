use super::*;
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
fn finalized_same_root_media_commits_one_path_update() {
    for buffered in [false, true] {
        verify_finalization(buffered, false);
    }
}

#[test]
fn finalized_distinct_root_media_keeps_relocation_update() {
    for buffered in [false, true] {
        verify_finalization(buffered, true);
    }
}

fn verify_finalization(buffered: bool, relocate: bool) {
    let mut config = storage_config(&format!("finalization-{buffered}-{relocate}"));
    let root = config.long_term_path.clone();
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    std::fs::create_dir_all(&root).unwrap();
    if relocate {
        config.medium_term_path = root.join("medium");
    }
    if !buffered {
        config.short_term_duration = Duration::ZERO;
        config.flush_interval = Duration::ZERO;
    }
    let catalog = RecordingCatalog::open(&config.recording_catalog_path).unwrap();
    let mut worker = WriterWorker::new(
        config.clone(),
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    let count = Arc::new(AtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(Updates(count.clone()));
    tracing::subscriber::with_default(subscriber, || {
        let started = Instant::now();
        for second in 0..3 {
            worker.ingest(
                RecordingStreamIdentity::legacy("front/main"),
                key_frame(started + Duration::from_secs(second)),
            );
        }
        worker.shutdown_flush();
        worker.finalize_all();
    });
    assert_eq!(count.load(Ordering::Relaxed), if relocate { 2 } else { 1 });
    assert_finalized_metadata(&config);
    drop(worker);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

fn assert_finalized_metadata(config: &StorageConfig) {
    pollster::block_on(async {
        let db = turso::Builder::new_local(config.recording_catalog_path.to_str().unwrap())
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let mut rows = conn.query("SELECT id,path,file_bytes,file_identity,started_at_ms,ended_at_ms,finalized,finalized_at_ms FROM recording_files", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let id = row.get::<String>(0).unwrap();
        let path = PathBuf::from(row.get::<String>(1).unwrap());
        assert!(path.starts_with(&config.long_term_path));
        assert_eq!(
            row.get::<i64>(2).unwrap(),
            i64::try_from(std::fs::metadata(&path).unwrap().len()).unwrap()
        );
        assert!(row.get::<Option<String>>(3).unwrap().is_some());
        let end = row.get::<i64>(5).unwrap();
        assert!(end > row.get::<i64>(4).unwrap());
        assert_eq!(row.get::<i64>(6).unwrap(), 1);
        assert!(row.get::<Option<i64>>(7).unwrap().is_some());
        assert!(rows.next().await.unwrap().is_none());
        let mut coverage = conn
            .query(
                "SELECT MAX(end_ms) FROM recording_coverage_ranges WHERE recording_id=?1",
                [id],
            )
            .await
            .unwrap();
        assert_eq!(
            coverage
                .next()
                .await
                .unwrap()
                .unwrap()
                .get::<i64>(0)
                .unwrap(),
            end
        );
    });
}
