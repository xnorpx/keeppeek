use super::*;
use crate::storage::{long_term::inspection::container, segment::RecordingRtpTime};
use std::io::Cursor;

fn worker_in(dir: &Path) -> WriterWorker {
    let mut config = storage_config("metadata-rotation");
    config.medium_term_path = dir.to_owned();
    config.long_term_path = dir.to_owned();
    WriterWorker::new(config, RecordingDemand::new(DEMAND_INACTIVITY_GRACE), None)
}

fn recovered_counts(root: &Path) -> Vec<(usize, u32)> {
    let files = LongTermStore::new(root.to_owned())
        .finalized_segments("camera")
        .unwrap();
    let mut result = Vec::new();
    for path in files {
        let bytes = std::fs::read(path).unwrap();
        let size = bytes.len() as u64;
        let index = container::inspect(
            &mut Cursor::new(&bytes),
            size,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        let reader = mp4::Mp4Reader::read_header(Cursor::new(&bytes), size).unwrap();
        result.push((index.fragments.len(), reader.tracks()[&1].sample_count()));
    }
    result.sort_unstable();
    result
}

#[test]
fn buffered_and_shutdown_batches_rotate_at_every_keyframe_without_source_metadata() {
    for shutdown in [false, true] {
        let dir = std::env::temp_dir().join(format!(
            "keeppeek-metadata-rotation-{}",
            uuid::Uuid::new_v4()
        ));
        let mut worker = worker_in(&dir);
        let identity = RecordingStreamIdentity::legacy("camera");
        let buffer = &mut worker.pipeline_for(&identity).short_term;
        let start = Instant::now() - Duration::from_secs(6);
        for n in 0..4_100 {
            buffer.push(key_frame(start + Duration::from_millis(n)));
        }
        if shutdown {
            worker.shutdown_flush();
        } else {
            assert!(worker.flush_camera("camera", true).unwrap().wrote);
        }
        worker.finalize_all();
        assert_eq!(recovered_counts(&dir), [(1_028, 1_028), (3_072, 3_072)]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn epoch_churn_preserves_the_prefix_before_the_hard_limit_and_resumes_at_a_keyframe() {
    let dir = std::env::temp_dir().join(format!(
        "keeppeek-metadata-rotation-{}",
        uuid::Uuid::new_v4()
    ));
    let mut worker = worker_in(&dir);
    let identity = RecordingStreamIdentity::legacy("camera");
    let buffer = &mut worker.pipeline_for(&identity).short_term;
    let start = Instant::now() - Duration::from_secs(115);
    for n in 0..=110_001_u64 {
        let at = start + Duration::from_millis(n);
        let mut frame = if matches!(n, 0 | 60_000 | 110_000) {
            key_frame(at)
        } else {
            let mut frame = inter_frame();
            frame.received_at = at;
            frame
        };
        frame.source_rtp = Some(RecordingRtpTime::from_packet(mp4::SourceRtpPacket {
            session_id: [1; 16],
            epoch_id: u128::from(n).to_be_bytes(),
            clock_rate: 90_000.try_into().unwrap(),
            timestamp: (n * 90) as i64,
        }));
        buffer.push(frame);
    }
    worker.shutdown_flush();
    worker.finalize_all();
    let files = recovered_counts(&dir);
    assert_eq!(files.len(), 2);
    assert_eq!(
        files[0],
        (1, 2),
        "the next keyframe starts a complete new file"
    );
    assert_eq!(
        files[1].0, 2,
        "the recording retains both admitted GOP prefixes"
    );
    assert!((60_001..110_000).contains(&files[1].1));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn buffered_receive_times_bound_segments_even_when_their_relocation_fails() {
    let dir = std::env::temp_dir().join(format!("keeppeek-aged-batch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut worker = worker_in(&dir);
    let unavailable = dir.join("occupied");
    std::fs::write(&unavailable, b"not a directory").unwrap();
    worker.config.long_term_path = unavailable.clone();
    worker.config.medium_term_duration = Duration::from_secs(60);
    worker.config.short_term_duration = Duration::from_secs(240);
    let identity = RecordingStreamIdentity::legacy("camera");
    let buffer = &mut worker.pipeline_for(&identity).short_term;
    let start = Instant::now() - Duration::from_secs(300);
    for seconds in [0, 30, 60, 90, 120, 150] {
        buffer.push(key_frame(start + Duration::from_secs(seconds)));
    }
    worker.shutdown_flush();
    worker.finalize_all();
    assert_eq!(recovered_counts(&dir), [(2, 2); 3]);
    worker
        .health
        .note_progress("camera", Duration::from_secs(150));
    assert!(
        worker.health.snapshot().streams[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("long-term storage")
    );
    assert_eq!(
        worker.health.snapshot().streams[0].recorded_duration_ms,
        150_000
    );

    // Even a new worker can rediscover the finalized source files without an in-memory queue.
    let mut recovered = WriterWorker::new(
        worker.config.clone(),
        RecordingDemand::new(Duration::ZERO),
        None,
    );
    std::fs::remove_file(&unavailable).unwrap();
    let mut now = Instant::now();
    for _ in 0..64 {
        let Some(delay) = recovered.retry_relocations(now) else {
            break;
        };
        now += delay;
    }
    assert_eq!(recovered_counts(&unavailable), [(2, 2); 3]);
    assert!(
        LongTermStore::new(dir.clone())
            .finalized_segments("camera")
            .unwrap()
            .is_empty()
    );
    for _ in 0..64 {
        let Some(delay) = worker.retry_relocations(now) else {
            break;
        };
        now += delay;
    }
    assert!(worker.health.snapshot().streams[0].last_error.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn health_includes_interframes_admitted_when_the_first_gop_activates() {
    for limit in [Duration::from_millis(100), Duration::from_secs(60)] {
        let dir =
            std::env::temp_dir().join(format!("keeppeek-gop-health-{}", uuid::Uuid::new_v4()));
        let mut worker = worker_in(&dir);
        worker.config.medium_term_duration = limit;
        let identity = RecordingStreamIdentity::legacy("camera");
        worker.pipeline_for(&identity);
        let start = Instant::now() - Duration::from_secs(2);
        for millis in [0, 40, 80] {
            let at = start + Duration::from_millis(millis);
            let mut frame = if millis == 0 {
                key_frame(at)
            } else {
                inter_frame()
            };
            frame.received_at = at;
            assert!(
                worker
                    .direct_write("camera", frame)
                    .unwrap()
                    .recorded_duration
                    .is_zero()
            );
        }
        let activated = worker
            .direct_write("camera", key_frame(start + Duration::from_millis(120)))
            .unwrap();
        assert!(activated.wrote);
        assert!(activated.recorded_duration >= Duration::from_millis(80));
        worker.finalize_all();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
