use super::*;

fn frame(start: Instant, ms: u64, key: bool, oversized: bool) -> RecordingFrame {
    let mut frame = key_frame(start + Duration::from_millis(ms));
    let MediaFrame::Video(video) = &mut frame.frame else {
        unreachable!("video fixture")
    };
    video.is_keyframe = key;
    if oversized {
        video.data = vec![0; 16 * 1024 * 1024].into();
    } else if !key {
        video.data = Bytes::from_static(&[0, 0, 0, 2, 0x41, 0x01]);
    }
    frame
}

#[test]
fn gap_accounts_for_prepared_media_and_stays_visible_after_buffered_recovery() {
    for direct in [true, false] {
        let mut config = storage_config(&format!("gop-gap-{}", uuid::Uuid::new_v4()));
        let root = config.long_term_path.clone();
        if direct {
            config.short_term_duration = Duration::ZERO;
            config.flush_interval = Duration::ZERO;
        }
        let mut worker = WriterWorker::new(config, RecordingDemand::new(Duration::ZERO), None);
        let source = RecordingStreamIdentity::legacy("camera/main");
        let start = Instant::now() - Duration::from_secs(120);
        for (at, key, oversized) in [(0, true, false), (100, false, false), (200, false, true)] {
            worker.ingest(source.clone(), frame(start, at, key, oversized));
        }
        if !direct {
            worker.ingest(source.clone(), frame(start, 300, true, false));
            worker.flush_all(false);
        }
        let health = worker
            .health
            .snapshot()
            .streams
            .into_iter()
            .find(|s| s.stream_id == source.storage_key)
            .unwrap();
        assert!(
            health
                .last_error
                .is_some_and(|e| e.contains("keyframe limit"))
        );
        assert!(
            health.recorded_duration_ms >= 100,
            "prepared media duration was lost on overflow"
        );
        for at in [400, 500] {
            worker.ingest(source.clone(), frame(start, at, true, false));
        }
        worker.flush_all(false);
        let health = worker
            .health
            .snapshot()
            .streams
            .into_iter()
            .find(|s| s.stream_id == source.storage_key)
            .unwrap();
        assert!(health.last_error.is_none());
        assert!(health.recorded_duration_ms >= 200);
        worker.finalize_all();
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn empty_rotations_into_distinct_long_term_roots_resume_in_both_writer_modes() {
    for direct in [true, false] {
        let mut config = storage_config(&format!("empty-gop-{}", uuid::Uuid::new_v4()));
        let root = config.long_term_path.clone();
        config.long_term_path = root.join("long");
        config.medium_term_path = root.join("medium");
        if direct {
            config.short_term_duration = Duration::ZERO;
            config.flush_interval = Duration::ZERO;
        }
        let demand = RecordingDemand::new(Duration::ZERO);
        let _viewer = demand.acquire("camera/main");
        let mut worker = WriterWorker::new(config, demand, None);
        let source = RecordingStreamIdentity::legacy("camera/main");
        let start = Instant::now() - Duration::from_secs(1);
        worker.ingest(source.clone(), frame(start, 0, true, true));
        let unused_path = worker.pipelines[&source.storage_key]
            .medium_term
            .as_ref()
            .unwrap()
            .active_path()
            .to_owned();
        assert!(!unused_path.exists());
        std::fs::write(&unused_path, b"unrelated").unwrap();
        worker.config.medium_term_duration = Duration::ZERO;
        worker.ingest(source.clone(), frame(start, 100, true, false));
        worker.config.medium_term_duration = Duration::from_secs(60);
        worker.ingest(source.clone(), frame(start, 200, false, false));
        let health = worker
            .health
            .snapshot()
            .streams
            .into_iter()
            .find(|s| s.stream_id == source.storage_key)
            .unwrap();
        assert!(health.last_error.is_none(), "{:?}", health.last_error);
        worker.finalize_all();
        let recordings = LongTermStore::new(root.join("long"))
            .finalized_segments("camera/main")
            .unwrap();
        assert_eq!(recordings.len(), 1);
        assert_eq!(std::fs::read(unused_path).unwrap(), b"unrelated");
        let media = mp4::read_mp4(File::open(&recordings[0]).unwrap()).unwrap();
        assert_eq!(media.tracks().values().next().unwrap().sample_count(), 2);
        drop(media);
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn single_gop_rotation_discontinuity_and_shutdown_publish_each_finalized_duration() {
    for direct in [true, false] {
        let mut config = storage_config(&format!("single-gop-health-{}", uuid::Uuid::new_v4()));
        let root = config.long_term_path.clone();
        config.medium_term_duration = Duration::from_millis(150);
        if direct {
            config.short_term_duration = Duration::ZERO;
            config.flush_interval = Duration::ZERO;
        }
        let mut worker = WriterWorker::new(config, RecordingDemand::new(Duration::ZERO), None);
        let source = RecordingStreamIdentity::legacy("camera/main");
        let start = Instant::now() - Duration::from_secs(4);
        let recorded_ms = |worker: &WriterWorker| {
            worker
                .health
                .snapshot()
                .streams
                .into_iter()
                .find(|s| s.stream_id == source.storage_key)
                .unwrap()
                .recorded_duration_ms
        };

        for (ms, key) in [(0, true), (100, false), (200, true), (300, false)] {
            worker.ingest(source.clone(), frame(start, ms, key, false));
        }
        worker.flush_all(true);
        assert_eq!(recorded_ms(&worker), 200, "rotation, direct={direct}");
        worker.finish_stream_before_gap(&source.storage_key);
        assert_eq!(recorded_ms(&worker), 400, "discontinuity, direct={direct}");
        for (ms, key) in [(400, true), (500, false)] {
            worker.ingest(source.clone(), frame(start, ms, key, false));
        }
        worker.flush_all(true);
        worker.finalize_all();
        assert_eq!(recorded_ms(&worker), 600, "shutdown, direct={direct}");
        let recordings = LongTermStore::new(root.clone())
            .finalized_segments(&source.storage_key)
            .unwrap();
        assert_eq!(recordings.len(), 3);
        for recording in recordings {
            let mut media = mp4::read_mp4(File::open(recording).unwrap()).unwrap();
            let (&id, track) = media.tracks().iter().next().unwrap();
            let timescale = track.timescale();
            assert_eq!(track.sample_count(), 2);
            let ticks = (1..=2)
                .map(|ordinal| u64::from(media.read_sample(id, ordinal).unwrap().unwrap().duration))
                .sum::<u64>();
            assert_eq!(ticks * 1_000 / u64::from(timescale), 200);
        }
        // A longer segment has already reported its first GOP through an ordinary append.
        worker.config.medium_term_duration = Duration::from_secs(60);
        for (ms, key) in [(600, true), (700, false), (800, true), (900, false)] {
            worker.ingest(source.clone(), frame(start, ms, key, false));
        }
        worker.flush_all(true);
        let active_duration = recorded_ms(&worker);
        assert!(active_duration > 600);
        worker.finalize_all();
        // Four 100 ms samples are now finalized, including the last sample of the open GOP.
        assert_eq!(recorded_ms(&worker), 1_000);
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn malformed_keyframe_keeps_the_committed_gop_health_credit() {
    for resume in [false, true] {
        let mut config =
            storage_config(&format!("failed-keyframe-health-{}", uuid::Uuid::new_v4()));
        let root = config.long_term_path.clone();
        config.short_term_duration = Duration::ZERO;
        config.flush_interval = Duration::ZERO;
        config.medium_term_duration = Duration::from_secs(60);
        let mut worker = WriterWorker::new(config, RecordingDemand::new(Duration::ZERO), None);
        let source = RecordingStreamIdentity::legacy("camera/main");
        let start = Instant::now();
        worker.ingest(source.clone(), frame(start, 0, true, false));
        worker.ingest(source.clone(), frame(start, 100, false, false));
        let mut broken = frame(start, 200, true, false);
        let MediaFrame::Video(video) = &mut broken.frame else {
            unreachable!()
        };
        video.codec = VideoCodec::H265;
        video.data = Bytes::from_static(&[0, 0, 0, 2, 0x26, 1]);
        worker.ingest(source.clone(), broken);
        assert!(worker.health.snapshot().streams[0].last_error.is_some());
        if resume {
            worker.ingest(source.clone(), frame(start, 300, true, false));
            worker.ingest(source.clone(), frame(start, 400, false, false));
            assert!(worker.health.snapshot().streams[0].recorded_duration_ms >= 200);
        }
        worker.finalize_all();
        let recordings = LongTermStore::new(root.clone())
            .finalized_segments(&source.storage_key)
            .unwrap();
        assert_eq!(recordings.len(), 1);
        let mut mp4 = mp4::read_mp4(File::open(&recordings[0]).unwrap()).unwrap();
        let (&track_id, track) = mp4.tracks().iter().next().unwrap();
        let timescale = track.timescale();
        let samples = track.sample_count();
        assert_eq!(samples, if resume { 4 } else { 2 });
        let ticks = (1..=samples)
            .map(|i| u64::from(mp4.read_sample(track_id, i).unwrap().unwrap().duration))
            .sum::<u64>();
        let actual = ticks * 1000 / u64::from(timescale);
        assert_eq!(actual, if resume { 400 } else { 200 });
        assert_eq!(
            worker.health.snapshot().streams[0].recorded_duration_ms,
            actual
        );
        drop(mp4);
        drop(worker);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
