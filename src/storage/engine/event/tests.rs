use super::super::tests::{key_frame, storage_config};
use super::*;

fn event_handle(capacity: usize, byte_limit: usize) -> (StorageHandle, StorageCommandReceiver) {
    let (tx, rx) = storage_command_channel(capacity, byte_limit);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventOnly,
        EventRecordingStream::Main,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    assert!(matches!(
        rx.try_recv().unwrap(),
        Command::ConfigureEventRecording { .. }
    ));
    (handle, rx)
}

#[test]
fn event_input_stays_charged_after_channel_receive() {
    let (handle, rx) = event_handle(4, 1024);
    let frame = key_frame(Instant::now());
    let bytes = frame.byte_len();
    handle.ingest_stream(RecordingStreamIdentity::legacy("camera/main"), frame);
    let received = rx.try_recv().unwrap();
    assert!(matches!(received, Command::EventInput { .. }));
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), bytes);
    drop(received);
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn concurrent_event_reconfiguration_keeps_frames_in_their_queued_epoch() {
    let (handle, rx) = event_handle(4096, 1024 * 1024);
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            for index in 0..128 {
                handle.configure_camera_event_recording(
                    "camera",
                    CameraRecordingMode::EventOnly,
                    if index % 2 == 0 {
                        EventRecordingStream::Sub
                    } else {
                        EventRecordingStream::Main
                    },
                    Duration::ZERO,
                    Duration::from_secs(1),
                );
                std::thread::yield_now();
            }
        });
        scope.spawn(|| {
            barrier.wait();
            for _ in 0..256 {
                for stream in ["main", "sub"] {
                    handle.ingest_stream(
                        RecordingStreamIdentity::new("camera", stream, "camera"),
                        key_frame(Instant::now()),
                    );
                }
                std::thread::yield_now();
            }
        });
    });
    let mut stream = EventRecordingStream::Main;
    let mut epoch = 0;
    let mut frames = 0;
    while let Ok(command) = rx.try_recv() {
        match command {
            Command::ConfigureEventRecording {
                settings, fence, ..
            } => {
                assert!(fence.observed > epoch);
                epoch = fence.observed;
                stream = settings.stream;
            }
            Command::EventInput {
                identity, fence, ..
            } => {
                assert_eq!(identity.stream_id, stream_name(stream));
                assert_eq!(fence.observed, epoch);
                frames += 1;
            }
            _ => panic!("only configuration and selected media were submitted"),
        }
    }
    assert!(frames > 0);
    assert_eq!(epoch, 128);
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn event_only_routes_selected_stream_and_zero_preroll_boost_uses_legacy_queue() {
    let (handle, rx) = event_handle(4, 1024);
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/sub"),
        key_frame(Instant::now()),
    );
    assert!(rx.try_recv().is_err());
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventBoost,
        EventRecordingStream::Main,
        Duration::ZERO,
        Duration::ZERO,
    );
    assert!(matches!(
        rx.try_recv().unwrap(),
        Command::ConfigureEventRecording { .. }
    ));
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/sub"),
        key_frame(Instant::now()),
    );
    assert!(matches!(rx.try_recv().unwrap(), Command::Ingest { .. }));
    assert!(!handle.admission.events_enabled.load(Ordering::Acquire));
}

#[test]
fn reconnect_invalidates_already_queued_input() {
    let (handle, rx) = event_handle(4, 1024);
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/main"),
        key_frame(Instant::now()),
    );
    let Command::EventInput { fence, input, .. } = rx.try_recv().unwrap() else {
        panic!("event input");
    };
    assert!(fence.valid());
    handle.reset_event_recording("camera");
    assert!(!fence.valid());
    drop(input);
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn privacy_on_then_off_rejects_queued_old_epoch() {
    let gate = Arc::new(PrivacyGate::new());
    let fence = EventFence {
        generations: Arc::new(EventGenerations::default()),
        observed: 0,
        main_observed: 0,
        privacy: Some((Arc::clone(&gate), gate.epoch())),
    };
    assert!(fence.valid());
    gate.activate();
    assert!(!fence.valid());
    gate.deactivate();
    assert!(!fence.valid());
}

#[test]
fn queue_pressure_releases_rejected_payload_and_fences_older_input() {
    let frame = key_frame(Instant::now());
    let bytes = frame.byte_len();
    let (handle, rx) = event_handle(4, bytes);
    handle.ingest_stream(RecordingStreamIdentity::legacy("camera/main"), frame);
    let Command::EventInput { fence, input, .. } = rx.try_recv().unwrap() else {
        panic!("event input");
    };
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/main"),
        key_frame(Instant::now()),
    );
    assert!(rx.try_recv().is_err());
    assert!(!fence.valid());
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), bytes);
    drop(input);
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn optional_main_queue_pressure_preserves_already_queued_continuous_sub() {
    let bytes = key_frame(Instant::now()).byte_len();
    let (tx, rx) = storage_command_channel(4, bytes);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventBoost,
        EventRecordingStream::Main,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    rx.try_recv().unwrap();
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/sub"),
        key_frame(Instant::now()),
    );
    let Command::EventInput { fence, input, .. } = rx.try_recv().unwrap() else {
        panic!("sub event input");
    };
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/main"),
        key_frame(Instant::now()),
    );
    assert!(rx.try_recv().is_err());
    assert!(
        fence.valid(),
        "optional main loss must preserve continuous sub"
    );
    drop(input);
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn optional_main_reserves_queue_slots_and_bytes_for_continuous_sub() {
    let bytes = key_frame(Instant::now()).byte_len();
    let (tx, rx) = storage_command_channel(4, 4 * bytes);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventBoost,
        EventRecordingStream::Main,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    rx.try_recv().unwrap();
    for _ in 0..3 {
        handle.ingest_stream(
            RecordingStreamIdentity::legacy("camera/main"),
            key_frame(Instant::now()),
        );
    }
    assert_eq!(handle.tx.optional_main_slots.load(Ordering::Relaxed), 2);
    assert_eq!(
        handle.tx.queued_media_bytes.load(Ordering::Relaxed),
        2 * bytes
    );
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/sub"),
        key_frame(Instant::now()),
    );
    for _ in 0..2 {
        let Command::EventInput {
            identity, fence, ..
        } = rx.try_recv().unwrap()
        else {
            panic!("main input")
        };
        assert_eq!(identity.stream_id, "main");
        assert!(!fence.main_valid());
    }
    let Command::EventInput {
        identity,
        fence,
        input,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!("sub input")
    };
    assert_eq!(identity.stream_id, "sub");
    assert!(fence.valid());
    drop(input);
    assert!(rx.try_recv().is_err());
    assert_eq!(handle.tx.optional_main_slots.load(Ordering::Relaxed), 0);
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn selected_replay_bypasses_short_term_aging_and_silent_deadline_finalizes() {
    for stream in [EventRecordingStream::Main, EventRecordingStream::Sub] {
        assert_silent_event_only_finalization(stream);
    }
}

fn assert_silent_event_only_finalization(stream: EventRecordingStream) {
    let config = storage_config(&format!("event-idle-deadline-{}", uuid::Uuid::new_v4()));
    let root = config.long_term_path.clone();
    let mut worker = WriterWorker::new(config, RecordingDemand::new(DEMAND_INACTIVITY_GRACE), None);
    let (tx, rx) = storage_command_channel(16, 1024);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventOnly,
        stream,
        Duration::from_secs(1),
        Duration::from_millis(50),
    );
    worker.handle_command(rx.try_recv().unwrap());
    assert!(worker.pipelines.is_empty());
    assert!(!root.exists());
    let start = Instant::now() - Duration::from_millis(200);
    let storage_key = format!("camera/{}", stream_name(stream));
    handle.ingest_stream(
        RecordingStreamIdentity::legacy(&storage_key),
        key_frame(start),
    );
    worker.handle_command(rx.try_recv().unwrap());
    assert!(worker.pipelines.is_empty());
    handle
        .admission
        .note_event(&handle.tx, "camera", start + Duration::from_millis(10));
    worker.handle_command(rx.try_recv().unwrap());
    for _ in 0..4 {
        worker.drain_event_outputs(Instant::now());
    }
    assert!(worker.pipelines.is_empty());
    let recordings = LongTermStore::new(root.clone())
        .finalized_segments(&storage_key)
        .unwrap();
    assert_eq!(recordings.len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn worker_rejects_queued_media_after_a_complete_privacy_cycle() {
    let config = storage_config(&format!("event-private-queue-{}", uuid::Uuid::new_v4()));
    let root = config.long_term_path.clone();
    let privacy = Arc::new(
        PrivacyRegistry::new(std::collections::BTreeMap::from([(
            "camera".to_owned(),
            crate::privacy::PrivacySchedule {
                enabled: false,
                timezone: "UTC".to_owned(),
                windows: Vec::new(),
                temporary_override: None,
                keep_camera_connected: true,
            },
        )]))
        .unwrap(),
    );
    let (tx, rx) = storage_command_channel(16, 1024);
    let admission = RecordingAdmission::default();
    *admission.privacy.write().unwrap() = Some(Arc::clone(&privacy));
    let mut worker = WriterWorker::new_with_health(
        config,
        RecordingDemand::new(DEMAND_INACTIVITY_GRACE),
        None,
        RecordingHealthRegistry::default(),
        Arc::clone(&admission.privacy),
    );
    let handle = StorageHandle { tx, admission };
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventOnly,
        EventRecordingStream::Main,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    worker.handle_command(rx.try_recv().unwrap());
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/main"),
        key_frame(Instant::now()),
    );
    let queued = rx.try_recv().unwrap();
    let gate = privacy.gate("camera").unwrap();
    gate.activate();
    gate.deactivate();
    worker.handle_command(queued);
    let status = worker
        .event_recordings
        .as_mut()
        .unwrap()
        .status("camera", Instant::now())
        .unwrap();
    assert_eq!(status.retained_bytes, 0);
    assert_eq!(status.reason, PreRecordReason::Privacy);
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), 0);
    assert!(worker.pipelines.is_empty());
    assert!(!root.exists());
}

#[test]
fn deadline_finalization_clamps_the_last_mp4_video_sample() {
    let root =
        storage_config(&format!("event-clamped-end-{}", uuid::Uuid::new_v4())).long_term_path;
    let start = Instant::now();
    let mut writer = MediumTermWriter::create(&root, "camera/main", start, 8192).unwrap();
    for offset in [0, 100, 200] {
        writer
            .append_received(key_frame(start + Duration::from_millis(offset)))
            .unwrap();
    }
    let path = writer
        .finalize_before(start + Duration::from_millis(250))
        .unwrap();
    let mut media = mp4::read_mp4(std::fs::File::open(path).unwrap()).unwrap();
    let track = *media.tracks().keys().next().unwrap();
    assert_eq!(media.tracks()[&track].sample_count(), 3);
    let last = media.read_sample(track, 3).unwrap().unwrap();
    assert_eq!(last.start_time, 18_000);
    assert_eq!(last.duration, 4_500);
    drop(media);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shutdown_commits_delayed_continuous_sub_without_waiting_for_the_horizon() {
    let config = storage_config(&format!("event-shutdown-{}", uuid::Uuid::new_v4()));
    let root = config.long_term_path.clone();
    let mut worker = WriterWorker::new(config, RecordingDemand::new(DEMAND_INACTIVITY_GRACE), None);
    let (tx, rx) = storage_command_channel(16, 4096);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventBoost,
        EventRecordingStream::Main,
        Duration::from_secs(30),
        Duration::from_secs(1),
    );
    worker.handle_command(rx.try_recv().unwrap());
    let start = Instant::now() - Duration::from_secs(1);
    for offset in [0, 100, 200] {
        handle.ingest_stream(
            RecordingStreamIdentity::legacy("camera/sub"),
            key_frame(start + Duration::from_millis(offset)),
        );
        worker.handle_command(rx.try_recv().unwrap());
    }
    assert_eq!(worker.drain_event_outputs(Instant::now()), 0);
    assert!(worker.pipelines.is_empty());
    assert!(worker.handle_command(Command::Shutdown));
    assert!(worker.event_recordings.is_none());
    assert!(
        worker
            .pipelines
            .values()
            .all(|pipeline| pipeline.medium_term.is_none())
    );
    let recordings = LongTermStore::new(root.clone())
        .finalized_segments("camera/sub")
        .unwrap();
    assert_eq!(recordings.len(), 1);
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn privacy_discard_preserves_written_fragments_without_writing_pending_video() {
    let root =
        storage_config(&format!("event-discard-pending-{}", uuid::Uuid::new_v4())).long_term_path;
    let start = Instant::now();
    let mut writer = MediumTermWriter::create(&root, "camera/main", start, 8192).unwrap();
    writer.append_received(key_frame(start)).unwrap();
    writer
        .append_received(key_frame(start + Duration::from_millis(100)))
        .unwrap();
    writer.discard_pending().unwrap();
    let path = writer.finalize().unwrap();
    let media = mp4::read_mp4(std::fs::File::open(path).unwrap()).unwrap();
    let track = media.tracks().values().next().unwrap();
    assert_eq!(track.sample_count(), 1);
    drop(media);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_keyframe_releases_a_replay_lease_when_the_history_budget_is_full() {
    let start = Instant::now() - Duration::from_millis(200);
    let bytes = key_frame(start).byte_len();
    let mut config = storage_config(&format!("event-budget-progress-{}", uuid::Uuid::new_v4()));
    config.pre_recording_stream_max_bytes = bytes;
    config.pre_recording_global_max_bytes = bytes;
    let root = config.long_term_path.clone();
    let mut worker = WriterWorker::new(config, RecordingDemand::new(DEMAND_INACTIVITY_GRACE), None);
    let (tx, rx) = storage_command_channel(16, 4096);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventOnly,
        EventRecordingStream::Main,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    worker.handle_command(rx.try_recv().unwrap());
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/main"),
        key_frame(start),
    );
    worker.handle_command(rx.try_recv().unwrap());
    handle.admission.note_event(&handle.tx, "camera", start);
    worker.handle_command(rx.try_recv().unwrap());
    worker.drain_event_outputs(start);
    let status = worker
        .event_recordings
        .as_mut()
        .unwrap()
        .status("camera", start)
        .unwrap();
    assert_eq!(status.retained_bytes, bytes as u64);
    let next = start + Duration::from_millis(100);
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/main"),
        key_frame(next),
    );
    worker.handle_command(rx.try_recv().unwrap());
    worker.drain_event_outputs(next);
    let status = worker
        .event_recordings
        .as_mut()
        .unwrap()
        .status("camera", next)
        .unwrap();
    assert_eq!(status.retained_bytes, 0);
    assert_eq!(handle.tx.queued_media_bytes.load(Ordering::Relaxed), 0);
    assert!(worker.handle_command(Command::Shutdown));
    assert_eq!(
        LongTermStore::new(root.clone())
            .finalized_segments("camera/main")
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn privacy_discard_during_preparation_leaves_no_recording_file() {
    let root =
        storage_config(&format!("event-discard-preparing-{}", uuid::Uuid::new_v4())).long_term_path;
    let start = Instant::now();
    let mut writer = MediumTermWriter::create(&root, "camera/main", start, 8192).unwrap();
    writer.append_received(key_frame(start)).unwrap();
    writer.discard_pending().unwrap();
    let path = writer.finalize().unwrap();
    assert!(!path.exists());
    assert!(
        LongTermStore::new(root.clone())
            .finalized_segments("camera/main")
            .unwrap()
            .is_empty()
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn configured_worker(
    label: &str,
) -> (StorageHandle, StorageCommandReceiver, WriterWorker, PathBuf) {
    let mut config = storage_config(&format!("{label}-{}", uuid::Uuid::new_v4()));
    config.short_term_duration = Duration::ZERO;
    let root = config.long_term_path.clone();
    let mut worker = WriterWorker::new(config, RecordingDemand::new(DEMAND_INACTIVITY_GRACE), None);
    let (tx, rx) = storage_command_channel(16, 4096);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventOnly,
        EventRecordingStream::Main,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    worker.handle_command(rx.try_recv().unwrap());
    (handle, rx, worker, root)
}

#[test]
fn writer_failure_clears_replay_without_stopping_another_continuous_camera() {
    let (handle, rx, mut worker, root) = configured_worker("event-writer-failure");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("camera"), b"not a directory").unwrap();
    let start = Instant::now() - Duration::from_millis(200);
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/main"),
        key_frame(start),
    );
    worker.handle_command(rx.try_recv().unwrap());
    handle.admission.note_event(&handle.tx, "camera", start);
    worker.handle_command(rx.try_recv().unwrap());
    worker.drain_event_outputs(Instant::now());
    let status = worker
        .event_recordings
        .as_mut()
        .unwrap()
        .status("camera", Instant::now())
        .unwrap();
    assert_eq!(status.reason, PreRecordReason::WriterFailure);
    assert_eq!(status.available_ms, 0);
    assert_eq!(status.retained_bytes, 0);
    for offset in [0, 100] {
        worker.ingest(
            RecordingStreamIdentity::legacy("other/sub"),
            key_frame(start + Duration::from_millis(offset)),
        );
    }
    assert!(worker.handle_command(Command::Shutdown));
    assert_eq!(
        LongTermStore::new(root.clone())
            .finalized_segments("other/sub")
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn storage_pause_discards_history_and_resume_requires_fresh_input() {
    let (handle, rx, mut worker, root) = configured_worker("event-storage-pause");
    let start = Instant::now() - Duration::from_millis(200);
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/main"),
        key_frame(start),
    );
    worker.handle_command(rx.try_recv().unwrap());
    worker.safety.cleanup_failed("test storage is full");
    worker.drain_event_outputs(Instant::now());
    let status = worker
        .event_recordings
        .as_mut()
        .unwrap()
        .status("camera", Instant::now())
        .unwrap();
    assert_eq!(status.reason, PreRecordReason::StoragePause);
    assert_eq!(status.retained_bytes, 0);
    worker.safety.cleanup_finished(0, 0);
    assert!(worker.sync_event_source("camera"));
    let status = worker
        .event_recordings
        .as_mut()
        .unwrap()
        .status("camera", Instant::now())
        .unwrap();
    assert_eq!(status.retained_bytes, 0);
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera/main"),
        key_frame(Instant::now()),
    );
    worker.handle_command(rx.try_recv().unwrap());
    let status = worker
        .event_recordings
        .as_mut()
        .unwrap()
        .status("camera", Instant::now())
        .unwrap();
    assert!(status.retained_bytes > 0);
    assert!(worker.pipelines.is_empty());
    assert!(!root.exists());
}

#[test]
fn configuring_zero_preroll_preserves_an_existing_legacy_writer() {
    let mut config = storage_config(&format!("event-legacy-config-{}", uuid::Uuid::new_v4()));
    config.short_term_duration = Duration::ZERO;
    config.flush_interval = Duration::ZERO;
    let root = config.long_term_path.clone();
    let mut worker = WriterWorker::new(config, RecordingDemand::new(DEMAND_INACTIVITY_GRACE), None);
    let start = Instant::now();
    worker.ingest(
        RecordingStreamIdentity::legacy("camera/sub"),
        key_frame(start),
    );
    let recording = worker.pipelines["camera/sub"]
        .medium_term
        .as_ref()
        .unwrap()
        .recording_id()
        .to_owned();
    let (tx, rx) = storage_command_channel(4, 4096);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    handle.configure_camera_event_recording(
        "camera",
        CameraRecordingMode::EventBoost,
        EventRecordingStream::Main,
        Duration::ZERO,
        Duration::from_secs(1),
    );
    worker.handle_command(rx.try_recv().unwrap());
    assert!(worker.event_recordings.is_none());
    assert_eq!(
        worker.pipelines["camera/sub"]
            .medium_term
            .as_ref()
            .unwrap()
            .recording_id(),
        recording
    );
    worker.handle_command(Command::Shutdown);
    assert_eq!(
        LongTermStore::new(root.clone())
            .finalized_segments("camera/sub")
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_programmatic_event_settings_disable_recording_instead_of_falling_back() {
    let (tx, rx) = storage_command_channel(4, 4096);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    for (pre, post) in [(1, 0), (1, 3601), (31, 1)] {
        handle.configure_camera_event_recording(
            "camera",
            CameraRecordingMode::EventOnly,
            EventRecordingStream::Main,
            Duration::from_secs(pre),
            Duration::from_secs(post),
        );
        let Command::ConfigureEventRecording { settings, .. } = rx.try_recv().unwrap() else {
            panic!("configuration")
        };
        assert_eq!(settings.mode, CameraRecordingMode::Off);
        handle.ingest_stream(
            RecordingStreamIdentity::legacy("camera/sub"),
            key_frame(Instant::now()),
        );
        handle.ingest_stream(
            RecordingStreamIdentity::legacy("camera/main"),
            key_frame(Instant::now()),
        );
        assert!(rx.try_recv().is_err());
        assert!(!handle.admission.events_enabled.load(Ordering::Acquire));
    }
}

#[test]
fn event_registry_capacity_rejection_disables_the_unregistered_camera() {
    let (tx, rx) = storage_command_channel(4, 4096);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    for index in 0..128 {
        let source = format!("camera{index}");
        handle.configure_camera_event_recording(
            &source,
            CameraRecordingMode::EventOnly,
            EventRecordingStream::Main,
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let Command::ConfigureEventRecording { settings, .. } = rx.try_recv().unwrap() else {
            panic!("configuration")
        };
        assert_eq!(
            settings.mode,
            if index < 127 {
                CameraRecordingMode::EventOnly
            } else {
                CameraRecordingMode::Off
            }
        );
    }
    handle.ingest_stream(
        RecordingStreamIdentity::legacy("camera127/sub"),
        key_frame(Instant::now()),
    );
    assert!(rx.try_recv().is_err());
}
