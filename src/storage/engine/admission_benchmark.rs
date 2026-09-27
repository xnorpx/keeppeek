//! Measurement-only access to the production admission path.
use super::*;
#[cfg(feature = "event-preroll-benchmark-current")]
pub mod sink;
use crate::storage::frame::{MediaFrame, VideoCodec, VideoFrame};
use bytes::Bytes;
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct FixtureFrame {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    pub data: Bytes,
    pub offset: Duration,
}

impl FixtureFrame {
    fn recording_frame(&self, start: Instant) -> RecordingFrame {
        RecordingFrame {
            received_at: start + self.offset,
            timestamp: None,
            frame: MediaFrame::Video(VideoFrame {
                codec: self.codec,
                width: self.width,
                height: self.height,
                is_keyframe: self.keyframe,
                data: self.data.clone(),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AdmissionReport {
    pub frames: usize,
    pub median_ingest_us: f64,
    pub p95_ingest_us: f64,
    pub total_work_ms: f64,
    pub peak_history_bytes: usize,
    pub peak_stream_bytes: usize,
    pub history_instances: usize,
    pub pending_channel_bytes: usize,
}

struct Consumer {
    short_term: HashMap<String, ShortTermBuffer>,
    peak_history_bytes: usize,
    peak_stream_bytes: usize,
    #[cfg(feature = "event-preroll-benchmark-current")]
    events: Option<EventRecordings>,
}

impl Consumer {
    fn new() -> Self {
        Self {
            short_term: HashMap::new(),
            peak_history_bytes: 0,
            peak_stream_bytes: 0,
            #[cfg(feature = "event-preroll-benchmark-current")]
            events: None,
        }
    }

    fn drain(&mut self, rx: &StorageCommandReceiver, now: Instant) {
        while let Ok(command) = rx.rx.try_recv() {
            rx.release_media_bytes(&command);
            match command {
                Command::Ingest {
                    identity, frame, ..
                } => self.push(identity, frame),
                #[cfg(feature = "event-preroll-benchmark-current")]
                command => self.event_command(command, now),
                #[cfg(not(feature = "event-preroll-benchmark-current"))]
                _ => {
                    let _ = now;
                }
            }
        }
        #[cfg(feature = "event-preroll-benchmark-current")]
        self.drain_events(now);
    }

    fn push(&mut self, identity: RecordingStreamIdentity, frame: RecordingFrame) {
        self.short_term
            .entry(identity.storage_key)
            .or_insert_with(|| ShortTermBuffer::new(Duration::from_secs(1)))
            .push(frame);
    }

    #[cfg(feature = "event-preroll-benchmark-current")]
    fn event_command(&mut self, command: Command, now: Instant) {
        match command {
            Command::ConfigureEventRecording {
                source, settings, ..
            } => {
                if settings.mode == CameraRecordingMode::EventBoost && !settings.pre.is_zero() {
                    let events = self.events.get_or_insert_with(|| {
                        EventRecordings::new(64 * 1024 * 1024, 256 * 1024 * 1024)
                    });
                    assert!(events.configure(&source, settings));
                }
            }
            Command::EventInput {
                identity, input, ..
            } => self.events.as_mut().unwrap().ingest(identity, *input, now),
            Command::RecordingEvent { source, at, .. } => {
                self.events.as_mut().unwrap().note_event(&source, at);
            }
            _ => {}
        }
        if let Some(events) = &self.events {
            self.peak_history_bytes = self.peak_history_bytes.max(events.retained_bytes());
            self.peak_stream_bytes = self.peak_stream_bytes.max(events.max_stream_bytes());
        }
    }

    #[cfg(feature = "event-preroll-benchmark-current")]
    fn drain_events(&mut self, now: Instant) {
        for _ in 0..131_072 {
            let Some(output) = self
                .events
                .as_mut()
                .and_then(|events| events.next_output(now))
            else {
                break;
            };
            if let EventOutput::Frame { identity, frame } = output {
                self.push(identity, frame);
            }
        }
    }

    fn history_instances(&self) -> usize {
        #[cfg(feature = "event-preroll-benchmark-current")]
        {
            usize::from(self.events.is_some())
        }
        #[cfg(not(feature = "event-preroll-benchmark-current"))]
        {
            0
        }
    }
}

fn configure(handle: &StorageHandle, source: &str, enabled: bool) {
    #[cfg(feature = "event-preroll-benchmark-current")]
    handle.configure_camera_event_recording(
        source,
        CameraRecordingMode::EventBoost,
        crate::cameras::EventRecordingStream::Main,
        Duration::from_secs(if enabled { 10 } else { 0 }),
        Duration::from_secs(2),
    );
    #[cfg(not(feature = "event-preroll-benchmark-current"))]
    {
        assert!(!enabled, "historical baseline cannot enable pre-roll");
        handle.configure_camera_recording(
            source,
            CameraRecordingMode::EventBoost,
            Duration::from_secs(2),
        );
    }
}

fn identities(
    handle: &StorageHandle,
    cameras: usize,
    enabled: bool,
) -> Vec<RecordingStreamIdentity> {
    (0..cameras)
        .flat_map(|camera| {
            let source = format!("camera-{camera:03}");
            configure(handle, &source, enabled);
            [
                RecordingStreamIdentity::new(&source, "sub", &source),
                RecordingStreamIdentity::new(&source, "main", &source),
            ]
        })
        .collect::<Vec<_>>()
}

fn trigger_overlap(
    handle: &StorageHandle,
    rx: &StorageCommandReceiver,
    consumer: &mut Consumer,
    fixture: &FixtureFrame,
    cameras: usize,
    now: Instant,
) {
    if matches!(fixture.offset.as_millis(), 6000 | 7000 | 8000) {
        for camera in 0..cameras {
            let source = format!("camera-{camera:03}");
            #[cfg(feature = "event-preroll-benchmark-current")]
            handle.admission.note_event(&handle.tx, &source, now);
            #[cfg(not(feature = "event-preroll-benchmark-current"))]
            handle.admission.note_event_at(&source, now);
        }
        consumer.drain(rx, now);
    }
}

pub fn measure_admission(
    frames: &[FixtureFrame],
    cameras: usize,
    enabled: bool,
) -> AdmissionReport {
    assert!(!frames.is_empty() && (1..=127).contains(&cameras));
    let (tx, rx) = storage_command_channel(COMMAND_CAPACITY, QUEUED_MEDIA_BYTES_CAPACITY);
    let handle = StorageHandle {
        tx,
        admission: RecordingAdmission::default(),
    };
    let identities = identities(&handle, cameras, enabled);
    let start = Instant::now();
    let mut consumer = Consumer::new();
    consumer.drain(&rx, start);
    let mut latencies = Vec::with_capacity(frames.len() * identities.len());
    let all_work = Instant::now();
    for fixture in frames {
        let now = start + fixture.offset;
        trigger_overlap(&handle, &rx, &mut consumer, fixture, cameras, now);
        for identity in &identities {
            let frame = fixture.recording_frame(start);
            let identity = identity.clone();
            let measured = Instant::now();
            handle.admission.ingest_at(&handle.tx, identity, frame, now);
            latencies.push(measured.elapsed().as_secs_f64() * 1_000_000.0);
            consumer.drain(&rx, now);
        }
    }
    let total_work_ms = all_work.elapsed().as_secs_f64() * 1000.0;
    latencies.sort_unstable_by(f64::total_cmp);
    let pending_channel_bytes = handle.tx.queued_media_bytes.load(Ordering::Relaxed);
    assert_eq!(pending_channel_bytes, 0);
    assert!(consumer.peak_history_bytes <= 256 * 1024 * 1024);
    assert!(consumer.peak_stream_bytes <= 64 * 1024 * 1024);
    assert!(enabled || consumer.history_instances() == 0);
    AdmissionReport {
        frames: latencies.len(),
        median_ingest_us: percentile(&latencies, 50),
        p95_ingest_us: percentile(&latencies, 95),
        total_work_ms,
        peak_history_bytes: consumer.peak_history_bytes,
        peak_stream_bytes: consumer.peak_stream_bytes,
        history_instances: consumer.history_instances(),
        pending_channel_bytes,
    }
}

const fn percentile(values: &[f64], percentile: usize) -> f64 {
    values[(values.len() * percentile).div_ceil(100).saturating_sub(1)]
}
