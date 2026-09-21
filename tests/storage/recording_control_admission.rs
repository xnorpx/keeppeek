use super::*;
use crate::storage::{
    MediaFrame, VideoCodec, VideoFrame,
    recording_control::{Override, Source},
};

fn clock(now: Instant, utc_ms: i64) -> Clock {
    Clock {
        monotonic: now,
        utc_ms: Some(utc_ms),
    }
}

fn frame(now: Instant, key: bool) -> RecordingFrame {
    RecordingFrame {
        received_at: now,
        timestamp: None,
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: key,
            width: 16,
            height: 16,
            data: vec![1, 2, 3].into(),
        }),
    }
}

fn disable() -> Override {
    Override {
        enabled: false,
        source: Source::Manual,
        actor: "admin".into(),
        reason: "inspection".into(),
        ttl_ms: 1_000,
    }
}

#[test]
fn control_expiry_observed_before_ingest_preserves_keyframe_and_output_discontinuity() {
    let admission = RecordingAdmission::default();
    admission.configure(
        "camera",
        CameraRecordingMode::EventBoost,
        Duration::from_secs(60),
    );
    let now = Instant::now();
    let (tx, rx) = storage_command_channel(1, 1024);
    let main = RecordingStreamIdentity::new("camera", "main", "camera");
    let sub = RecordingStreamIdentity::new("camera", "sub", "camera");
    admission.note_event_at("camera", now);
    admission.ingest_at(&tx, main, frame(now, true), now);
    let revision = admission
        .control_snapshot("camera", clock(now, 10_000))
        .unwrap()
        .revision;
    admission
        .set_override("camera", revision, disable(), clock(now, 10_000))
        .unwrap();
    admission.configure(
        "camera",
        CameraRecordingMode::EventBoost,
        Duration::from_secs(60),
    );
    admission.note_event("camera", Some(clock(now, 10_500)));
    admission.ingest_with_clock(
        &tx,
        sub.clone(),
        frame(now, true),
        Some(clock(now, 10_500)),
        || {},
    );
    assert_eq!(
        admission
            .control_snapshot("camera", clock(now, 10_500))
            .unwrap()
            .mode,
        CameraRecordingMode::Off
    );
    // The status read expires the request before another frame arrives.
    let later = now + Duration::from_secs(1);
    admission
        .control_snapshot("camera", clock(later, 11_000))
        .unwrap();
    admission.ingest_at(&tx, sub.clone(), frame(later, false), later);
    // A full queue rejects this recovery keyframe and must keep its discontinuity marker.
    admission.ingest_at(&tx, sub.clone(), frame(later, true), later);
    let Command::Ingest { identity, .. } = rx.try_recv().unwrap() else {
        panic!()
    };
    assert_eq!(identity.stream_id, "sub");
    admission.ingest_at(&tx, sub.clone(), frame(later, false), later);
    assert!(rx.try_recv().is_err());
    admission.ingest_at(&tx, sub, frame(later, true), later);
    let Command::Ingest {
        identity,
        discontinuity,
        frame,
    } = rx.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!(identity.storage_key, "camera/sub");
    assert!(frame.is_video_keyframe());
    assert!(discontinuity);
}

#[test]
fn privacy_suppresses_events_and_reconfiguration_preserves_the_bound() {
    let admission = RecordingAdmission::default();
    let now = Instant::now();
    let (tx, rx) = storage_command_channel(8, 1024);
    admission.configure(
        "camera",
        CameraRecordingMode::EventBoost,
        Duration::from_secs(60),
    );
    admission
        .set_privacy("camera", Some(true), clock(now, 10_000))
        .unwrap();
    admission.configure(
        "camera",
        CameraRecordingMode::EventBoost,
        Duration::from_secs(60),
    );
    admission.note_event_at("camera", now);
    admission.ingest_at(
        &tx,
        RecordingStreamIdentity::new("camera", "main", "camera"),
        frame(now, true),
        now,
    );
    assert!(rx.try_recv().is_err());
    admission
        .set_privacy("camera", Some(false), clock(now, 10_001))
        .unwrap();
    admission.ingest_at(
        &tx,
        RecordingStreamIdentity::new("camera", "main", "camera"),
        frame(now, true),
        now,
    );
    assert!(rx.try_recv().is_err());
    admission.ingest_at(
        &tx,
        RecordingStreamIdentity::new("camera", "sub", "camera"),
        frame(now, false),
        now,
    );
    assert!(rx.try_recv().is_err());
    admission.ingest_at(
        &tx,
        RecordingStreamIdentity::new("camera", "sub", "camera"),
        frame(now, true),
        now,
    );
    let Command::Ingest { discontinuity, .. } = rx.try_recv().unwrap() else {
        panic!()
    };
    assert!(discontinuity);
}

#[test]
fn fixed_clock_disable_rejects_frames_and_events_until_expiry() {
    let admission = RecordingAdmission::default();
    admission.configure(
        "camera",
        CameraRecordingMode::EventBoost,
        Duration::from_secs(60),
    );
    let now = Instant::now();
    let (tx, rx) = storage_command_channel(8, 1024);
    let revision = admission
        .control_snapshot("camera", clock(now, 10_000))
        .unwrap()
        .revision;
    admission
        .set_override("camera", revision, disable(), clock(now, 10_000))
        .unwrap();
    let before = clock(now + Duration::from_millis(999), 10_999);
    admission.note_event("camera", Some(before));
    for stream in ["main", "sub"] {
        admission.ingest_with_clock(
            &tx,
            RecordingStreamIdentity::new("camera", stream, "camera"),
            frame(before.monotonic, true),
            Some(before),
            || {},
        );
    }
    assert!(rx.try_recv().is_err());
    let expired = clock(now + Duration::from_secs(1), 11_000);
    for stream in ["main", "sub"] {
        admission.ingest_with_clock(
            &tx,
            RecordingStreamIdentity::new("camera", stream, "camera"),
            frame(expired.monotonic, true),
            Some(expired),
            || {},
        );
    }
    let Command::Ingest {
        identity,
        discontinuity,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!()
    };
    assert_eq!(identity.stream_id, "sub");
    assert!(discontinuity);
    assert!(rx.try_recv().is_err());
}
