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

#[test]
fn restarted_catalog_recovers_failed_relocation_without_replacing_other_bytes() {
    let mut config = storage_config(&format!("relocation-retry-{}", uuid::Uuid::new_v4()));
    let root = config.long_term_path.clone();
    config.medium_term_path = root.join("medium");
    config.long_term_path = root.join("archive");
    std::fs::create_dir_all(&config.medium_term_path).unwrap();
    std::fs::write(&config.long_term_path, b"offline").unwrap();
    let catalog = RecordingCatalog::open(&config.recording_catalog_path).unwrap();
    let mut worker = WriterWorker::new(
        config.clone(),
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    let started = Instant::now() - Duration::from_secs(4);
    for second in 0..3 {
        worker.ingest(
            RecordingStreamIdentity::legacy("front/main"),
            key_frame(started + Duration::from_secs(second)),
        );
    }
    worker.shutdown_flush();
    worker.finalize_all();
    let files = LongTermStore::new(config.medium_term_path.clone())
        .finalized_segments("front/main")
        .unwrap();
    assert_eq!(files.len(), 1);
    let source = &files[0];
    let original = std::fs::read(source).unwrap();
    let destination = config
        .long_term_path
        .join(source.strip_prefix(&config.medium_term_path).unwrap());
    std::fs::remove_file(&config.long_term_path).unwrap();
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let different = vec![0x5a; original.len()];
    std::fs::write(&destination, &different).unwrap();
    drop(worker);
    let mut worker = WriterWorker::new(
        config.clone(),
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    let mut now = Instant::now();
    for _ in 0..12 {
        let delay = worker.retry_relocations(now).unwrap();
        now += delay;
    }
    assert_eq!(std::fs::read(&destination).unwrap(), different);
    assert_eq!(std::fs::read(source).unwrap(), original);
    assert!(worker.health.snapshot().streams[0].last_error.is_some());
    std::fs::remove_file(&destination).unwrap();

    let (fragments, reader) = catalog
        .handle()
        .leased_media_fragments_in_range("front/main", 0, i64::MAX)
        .unwrap();
    assert!(!fragments.is_empty());
    assert!(
        fragments
            .iter()
            .all(|f| std::path::Path::new(&f.path) == source)
    );
    // A previous process may have staged the correct bytes just before losing its
    // catalog update. Playback still owns the old path until the reader closes.
    std::fs::write(&destination, &original).unwrap();
    for _ in 0..12 {
        let delay = worker.retry_relocations(now).unwrap();
        now += delay;
    }
    assert_eq!(std::fs::read(source).unwrap(), original);
    assert_eq!(std::fs::read(&destination).unwrap(), original);
    drop(reader);
    for _ in 0..64 {
        let Some(delay) = worker.retry_relocations(now) else {
            break;
        };
        now += delay;
    }
    assert_eq!(std::fs::read(&destination).unwrap(), original);
    assert!(!source.exists());
    assert!(worker.health.snapshot().streams[0].last_error.is_none());
    assert_finalized_metadata(&config);
    drop(worker);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}
