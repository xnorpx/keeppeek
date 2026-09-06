use super::*;
use crate::storage::{RecordingCatalog, metadata::EventSource};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn with_native_recorder(test: impl FnOnce(KeepPeekLoop, &EventStore)) {
    let directory = std::env::temp_dir().join(format!("keeppeek-native-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
    let store = EventStore::new(catalog.handle(), &directory.join("images"), 0).unwrap();
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    recorder.set_event_store(store.clone());
    test(recorder, &store);
    drop(store);
    catalog.shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}

fn native_batch(
    recorder: &mut KeepPeekLoop,
    owner: uuid::Uuid,
    lifetime: &Arc<AtomicBool>,
    changes: Vec<KeepPeekEvent>,
) -> usize {
    let (reply, received) = mpsc::sync_channel(1);
    recorder.handle_event(KeepPeekEvent::NativeBatch {
        owner,
        lifetime: Arc::clone(lifetime),
        changes,
        reply,
    });
    received.recv_timeout(Duration::from_secs(1)).unwrap()
}

fn commit_native_changes(
    recorder: &mut KeepPeekLoop,
    owner: uuid::Uuid,
    lifetime: &Arc<AtomicBool>,
    changes: &[KeepPeekEvent],
) {
    let mut completed = 0;
    for _ in 0..changes.len() {
        if completed == changes.len() {
            break;
        }
        let prefix = native_batch(recorder, owner, lifetime, changes[completed..].to_vec());
        assert!(prefix > 0 && prefix <= changes.len() - completed);
        completed += prefix;
    }
    assert_eq!(completed, changes.len());
}

fn motion(id: &str) -> KeepPeekEvent {
    KeepPeekEvent::TimelineEventStarted {
        event: Box::new(TimelineEvent {
            id: id.to_owned(),
            revision: 1,
            camera_id: "127.0.0.1".to_owned(),
            stream: None,
            source: EventSource::Camera,
            kind: "motion".to_owned(),
            start_time_ms: 1000,
            end_time_ms: None,
            confidence: None,
            bbox: None,
            bbox_attachment_id: None,
            zone: None,
            text: None,
            payload: None,
            attachments: Vec::new(),
            canonical_attachment_id: None,
            icon_key: "motion".to_owned(),
            rejected_icon_key: None,
            thumbnail_filename: None,
        }),
    }
}

fn observed_motion(
    store: &EventStore,
    id: &str,
    observation_time_ms: i64,
    trusted: bool,
) -> KeepPeekEvent {
    let mut event = store.event_by_id(id).unwrap().unwrap();
    let camera_time = if trusted {
        chrono::DateTime::from_timestamp_millis(observation_time_ms)
            .unwrap()
            .to_rfc3339()
    } else {
        "2099-01-01T00:00:00Z".to_owned()
    };
    event.confidence = Some(0.9);
    event.payload = Some(serde_json::Map::from_iter([
        ("protocol".to_owned(), serde_json::json!("onvif")),
        (
            "observation_time_ms".to_owned(),
            serde_json::json!(observation_time_ms),
        ),
        (
            "timestamp_source".to_owned(),
            serde_json::json!(if trusted { "camera" } else { "received" }),
        ),
        (
            "timestamp_reason".to_owned(),
            serde_json::json!((!trusted).then_some("camera_time_out_of_window")),
        ),
        ("cameraTime".to_owned(), serde_json::json!(camera_time)),
    ]));
    KeepPeekEvent::TimelineEventImages {
        event: Box::new(event),
        images: Vec::new(),
    }
}

#[test]
fn final_native_shutdown_closes_all_retired_events_once() {
    with_native_recorder(|mut recorder, store| {
        let (sent, published) = mpsc::channel();
        recorder.set_event_publisher(move |event| {
            sent.send(event.clone()).unwrap();
        });
        let lifetimes = [
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
        ];
        let mut ids = Vec::with_capacity(256);
        for lifetime in &lifetimes {
            let owner = uuid::Uuid::new_v4();
            let changes: Vec<_> = (0..128)
                .map(|index| {
                    let id = format!("{owner}-{index}");
                    let change = motion(&id);
                    ids.push(id);
                    change
                })
                .collect();
            commit_native_changes(&mut recorder, owner, lifetime, &changes);
        }
        assert_eq!(published.try_iter().count(), 256);
        for lifetime in lifetimes {
            lifetime.store(false, Ordering::Release);
        }
        recorder.shutdown.cancel();
        let started = Instant::now();
        recorder.run();
        let elapsed = started.elapsed();
        let endings: Vec<_> = published.try_iter().collect();
        assert_eq!(endings.len(), 256, "native shutdown elapsed: {elapsed:?}");
        for id in ids {
            let stored = store.event_by_id(&id).unwrap().unwrap();
            assert_eq!(stored.end_time_ms, Some(1000));
            assert_eq!(stored.revision, 2);
            assert_eq!(endings.iter().filter(|event| event.id == id).count(), 1);
        }
    });
}

#[test]
fn native_close_returns_only_the_new_committed_revision() {
    with_native_recorder(|mut recorder, store| {
        let owner = uuid::Uuid::new_v4();
        let lifetime = Arc::new(AtomicBool::new(true));
        commit_native_changes(&mut recorder, owner, &lifetime, &[motion("close-once")]);
        assert!(store.close_native_event("close-once", 999).is_err());
        let open = store.event_by_id("close-once").unwrap().unwrap();
        assert_eq!(open.revision, 1);
        assert!(open.end_time_ms.is_none());
        let closed = store
            .close_native_event("close-once", 2000)
            .unwrap()
            .unwrap();
        assert_eq!(closed.revision, 2);
        assert_eq!(closed.end_time_ms, Some(2000));
        assert_eq!(Some(closed), store.event_by_id("close-once").unwrap());
        assert!(
            store
                .close_native_event("close-once", 3000)
                .unwrap()
                .is_none()
        );
        assert!(store.close_native_event("missing", 2000).unwrap().is_none());
        assert_eq!(
            store.event_by_id("close-once").unwrap().unwrap().revision,
            2
        );
    });
}

#[test]
fn periodic_native_retirement_keeps_64_event_budget() {
    with_native_recorder(|mut recorder, store| {
        let owner = uuid::Uuid::new_v4();
        let lifetime = Arc::new(AtomicBool::new(true));
        let changes: Vec<_> = (0..70)
            .map(|index| motion(&format!("periodic-{index}")))
            .collect();
        commit_native_changes(&mut recorder, owner, &lifetime, &changes);
        let (sent, published) = mpsc::channel();
        recorder.set_event_publisher(move |event| {
            sent.send(event.clone()).unwrap();
        });
        lifetime.store(false, Ordering::Release);
        recorder.close_retired_native_events();
        assert_eq!(published.try_iter().count(), 64);
        assert_eq!(recorder.drain_retired_native_events(), 0);
        assert_eq!(published.try_iter().count(), 6);
        assert_eq!(recorder.drain_retired_native_events(), 0);
        assert_eq!(published.try_iter().count(), 0);
        for index in 0..70 {
            let stored = store
                .event_by_id(&format!("periodic-{index}"))
                .unwrap()
                .unwrap();
            assert_eq!(stored.end_time_ms, Some(1000));
            assert_eq!(stored.revision, 2);
        }
    });
}

#[test]
fn final_native_drain_stops_without_progress_and_retains_all_pending_ids() {
    with_native_recorder(|mut recorder, store| {
        let owner = uuid::Uuid::new_v4();
        let lifetime = Arc::new(AtomicBool::new(true));
        let changes: Vec<_> = (0..128)
            .map(|index| motion(&format!("pending-{index}")))
            .collect();
        commit_native_changes(&mut recorder, owner, &lifetime, &changes);
        let available = recorder.events.take().unwrap();
        lifetime.store(false, Ordering::Release);
        let started = Instant::now();
        assert_eq!(recorder.drain_retired_native_events(), 128);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(recorder.drain_retired_native_events(), 128);
        for index in 0..128 {
            assert!(
                store
                    .event_by_id(&format!("pending-{index}"))
                    .unwrap()
                    .unwrap()
                    .end_time_ms
                    .is_none()
            );
        }
        recorder.set_event_store(available);
        let started = Instant::now();
        let remaining = recorder.drain_retired_native_events();
        assert_eq!(
            remaining,
            0,
            "recovered native drain elapsed: {:?}",
            started.elapsed()
        );
        for index in 0..128 {
            let stored = store
                .event_by_id(&format!("pending-{index}"))
                .unwrap()
                .unwrap();
            assert_eq!(stored.end_time_ms, Some(1000));
            assert_eq!(stored.revision, 2);
        }
    });
}

#[test]
fn final_native_drain_checks_deadline_between_backpressured_commits() {
    with_native_recorder(|mut recorder, store| {
        let owner = uuid::Uuid::new_v4();
        let lifetime = Arc::new(AtomicBool::new(true));
        let changes: Vec<_> = (0..3)
            .map(|index| motion(&format!("budget-{index}")))
            .collect();
        commit_native_changes(&mut recorder, owner, &lifetime, &changes);
        lifetime.store(false, Ordering::Release);
        assert_eq!(
            recorder.drain_retired_native_events_until(Instant::now()),
            3
        );
        let (sent, published) = mpsc::channel();
        let (_release, blocked) = mpsc::sync_channel::<()>(1);
        let blocked = std::sync::Mutex::new(blocked);
        let deadline = Instant::now() + Duration::from_millis(250);
        recorder.set_event_publisher(move |event| {
            sent.send(event.clone()).unwrap();
            let result = blocked
                .lock()
                .unwrap()
                .recv_timeout(deadline.saturating_duration_since(Instant::now()));
            assert_eq!(result, Err(mpsc::RecvTimeoutError::Timeout));
        });
        assert_eq!(recorder.drain_retired_native_events_until(deadline), 2);
        assert_eq!(published.try_iter().count(), 1);
        recorder.set_event_publisher(|_| {});
        assert_eq!(recorder.drain_retired_native_events(), 0);
        for index in 0..3 {
            let stored = store
                .event_by_id(&format!("budget-{index}"))
                .unwrap()
                .unwrap();
            assert_eq!(stored.end_time_ms, Some(1000));
            assert_eq!(stored.revision, 2);
        }
    });
}

#[test]
fn retired_native_event_uses_effective_payload_observation_time() {
    for trusted in [true, false] {
        with_native_recorder(|mut recorder, store| {
            let owner = uuid::Uuid::new_v4();
            let lifetime = Arc::new(AtomicBool::new(true));
            commit_native_changes(&mut recorder, owner, &lifetime, &[motion("observed")]);
            for observation_time_ms in [2000_i64, 1500] {
                commit_native_changes(
                    &mut recorder,
                    owner,
                    &lifetime,
                    &[observed_motion(
                        store,
                        "observed",
                        observation_time_ms,
                        trusted,
                    )],
                );
            }
            lifetime.store(false, Ordering::Release);
            assert_eq!(recorder.drain_retired_native_events(), 0);
            let stored = store.event_by_id("observed").unwrap().unwrap();
            assert_eq!(stored.start_time_ms, 1000);
            assert_eq!(stored.end_time_ms, Some(2000));
            assert_eq!(stored.confidence, Some(0.9));
            assert_eq!(stored.revision, 4);
        });
    }
}

#[test]
fn retired_native_event_retains_failed_ending_only_for_owning_lifetime() {
    with_native_recorder(|mut recorder, store| {
        let owner = uuid::Uuid::new_v4();
        let lifetime = Arc::new(AtomicBool::new(true));
        commit_native_changes(&mut recorder, owner, &lifetime, &[motion("ending")]);
        let available = recorder.events.take().unwrap();
        let unrelated = Arc::new(AtomicBool::new(true));
        for (batch_owner, batch_lifetime, id, end_time_ms) in [
            (owner, &lifetime, "ending", 3000),
            (uuid::Uuid::new_v4(), &lifetime, "ending", 9000),
            (owner, &unrelated, "ending", 8000),
            (owner, &lifetime, "unknown", 7000),
        ] {
            let ending = KeepPeekEvent::TimelineEventEnded {
                id: id.to_owned(),
                end_time_ms,
            };
            assert_eq!(
                native_batch(&mut recorder, batch_owner, batch_lifetime, vec![ending]),
                0
            );
        }
        assert!(
            store
                .event_by_id("ending")
                .unwrap()
                .unwrap()
                .end_time_ms
                .is_none()
        );
        lifetime.store(false, Ordering::Release);
        assert_eq!(recorder.drain_retired_native_events(), 1);
        recorder.set_event_store(available);
        assert_eq!(recorder.drain_retired_native_events(), 0);
        let stored = store.event_by_id("ending").unwrap().unwrap();
        assert_eq!(stored.end_time_ms, Some(3000));
        assert_eq!(stored.revision, 2);
        assert!(store.event_by_id("unknown").unwrap().is_none());
    });
}

#[test]
fn retired_native_event_uses_last_durable_observation_when_update_cannot_commit() {
    with_native_recorder(|mut recorder, store| {
        let owner = uuid::Uuid::new_v4();
        let lifetime = Arc::new(AtomicBool::new(true));
        commit_native_changes(&mut recorder, owner, &lifetime, &[motion("durable")]);
        commit_native_changes(
            &mut recorder,
            owner,
            &lifetime,
            &[observed_motion(store, "durable", 2000, false)],
        );
        let available = recorder.events.take().unwrap();
        let update = observed_motion(store, "durable", 5000, false);
        assert_eq!(
            native_batch(&mut recorder, owner, &lifetime, vec![update]),
            0
        );
        lifetime.store(false, Ordering::Release);
        recorder.set_event_store(available);
        assert_eq!(recorder.drain_retired_native_events(), 0);
        let stored = store.event_by_id("durable").unwrap().unwrap();
        assert_eq!(stored.end_time_ms, Some(2000));
        assert_eq!(stored.payload.unwrap()["observation_time_ms"], 2000);
        assert_eq!(stored.revision, 3);
    });
}

#[test]
fn retired_native_event_without_effective_time_does_not_trust_camera_time() {
    with_native_recorder(|mut recorder, store| {
        let owner = uuid::Uuid::new_v4();
        let lifetime = Arc::new(AtomicBool::new(true));
        commit_native_changes(&mut recorder, owner, &lifetime, &[motion("legacy")]);
        let mut update = observed_motion(store, "legacy", 2000, false);
        let KeepPeekEvent::TimelineEventImages { event, .. } = &mut update else {
            panic!("expected native update");
        };
        event
            .payload
            .as_mut()
            .unwrap()
            .remove("observation_time_ms");
        commit_native_changes(&mut recorder, owner, &lifetime, &[update]);
        lifetime.store(false, Ordering::Release);
        assert_eq!(recorder.drain_retired_native_events(), 0);
        let stored = store.event_by_id("legacy").unwrap().unwrap();
        assert_eq!(stored.end_time_ms, Some(1000));
        assert_eq!(stored.revision, 3);
    });
}

#[test]
fn native_retirement_fences_partial_and_unknown_ack_batches() {
    for lose_ack in [false, true] {
        with_native_recorder(|mut recorder, store| {
            let owner = uuid::Uuid::new_v4();
            let lifetime = Arc::new(AtomicBool::new(true));
            let retired = Arc::clone(&lifetime);
            let (sent, published) = mpsc::channel();
            recorder.set_event_publisher(move |event| {
                retired.store(false, Ordering::Release);
                sent.send(event.clone()).unwrap();
            });
            let (reply, received) = mpsc::sync_channel(1);
            let received = (!lose_ack).then_some(received);
            recorder.handle_event(KeepPeekEvent::NativeBatch {
                owner,
                lifetime: Arc::clone(&lifetime),
                changes: vec![motion("committed-prefix"), motion("uncommitted-suffix")],
                reply,
            });
            if let Some(received) = received {
                assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
            }
            assert_eq!(
                native_batch(&mut recorder, owner, &lifetime, vec![motion("late")]),
                0
            );
            assert_eq!(recorder.drain_retired_native_events(), 0);
            let stored = store.event_by_id("committed-prefix").unwrap().unwrap();
            assert_eq!(stored.end_time_ms, Some(1000));
            assert_eq!(stored.revision, 2);
            assert!(store.event_by_id("uncommitted-suffix").unwrap().is_none());
            assert!(store.event_by_id("late").unwrap().is_none());
            let published: Vec<_> = published.try_iter().collect();
            assert_eq!(published.len(), 2);
            assert_eq!(
                published
                    .iter()
                    .filter(|event| event.end_time_ms.is_some())
                    .count(),
                1
            );
        });
    }
}

#[test]
fn retired_native_owner_rejects_late_starts_and_closes_committed_events() {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-native-owner-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
    let store = EventStore::new(catalog.handle(), &directory.join("images"), 0).unwrap();
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    recorder.set_event_store(store.clone());
    let lifetime = Arc::new(AtomicBool::new(true));
    let owner = uuid::Uuid::new_v4();
    let (reply, received) = mpsc::sync_channel(1);
    recorder.handle_event(KeepPeekEvent::NativeBatch {
        owner,
        lifetime: Arc::clone(&lifetime),
        changes: vec![motion("committed")],
        reply,
    });
    assert_eq!(received.recv().unwrap(), 1);
    assert!(
        store
            .event_by_id("committed")
            .unwrap()
            .unwrap()
            .end_time_ms
            .is_none()
    );
    lifetime.store(false, Ordering::Release);
    let (reply, received) = mpsc::sync_channel(1);
    recorder.handle_event(KeepPeekEvent::NativeBatch {
        owner,
        lifetime,
        changes: vec![motion("late")],
        reply,
    });
    assert_eq!(received.recv().unwrap(), 0);
    recorder.close_retired_native_events();
    assert!(store.event_by_id("late").unwrap().is_none());
    assert_eq!(
        store.event_by_id("committed").unwrap().unwrap().end_time_ms,
        Some(1000)
    );
    drop(recorder);
    drop(store);
    catalog.shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn optional_native_snapshot_failure_does_not_block_lifecycle_commits() {
    let directory = std::env::temp_dir().join(format!(
        "keeppeek-native-snapshot-failure-{}",
        uuid::Uuid::new_v4()
    ));
    let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
    let images = directory.join("images");
    let store = EventStore::new(catalog.handle(), &images, 0).unwrap();
    std::fs::rename(&images, directory.join("original-images")).unwrap();
    std::fs::write(&images, b"blocked snapshot destination").unwrap();
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(16, 16)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    let mut recorder = KeepPeekLoop::new(Shutdown::new(), None);
    recorder.set_event_store(store.clone());
    let (reply, received) = mpsc::sync_channel(1);
    recorder.handle_event(KeepPeekEvent::NativeBatch {
        owner: uuid::Uuid::new_v4(),
        lifetime: Arc::new(AtomicBool::new(true)),
        reply,
        changes: vec![
            motion("first"),
            KeepPeekEvent::TimelineEventThumbnail {
                camera_id: "127.0.0.1".to_owned(),
                event_id: "first".to_owned(),
                jpeg: jpeg.into_inner(),
            },
            KeepPeekEvent::TimelineEventEnded {
                id: "first".to_owned(),
                end_time_ms: 2000,
            },
            motion("second"),
        ],
    });
    assert_eq!(received.recv().unwrap(), 4);
    let first = store.event_by_id("first").unwrap().unwrap();
    assert_eq!(first.end_time_ms, Some(2000));
    assert!(first.attachments.is_empty());
    assert!(store.event_by_id("second").unwrap().is_some());
    drop(recorder);
    drop(store);
    catalog.shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}
