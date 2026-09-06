use std::collections::VecDeque;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, SyncSender},
};
use std::time::{Duration, Instant};

use super::{
    lifecycle::Tracker,
    registry::{Input, Slot},
};
use crate::{cameras::Camera, keeppeek::KeepPeekEvent, shutdown::Shutdown};

const PENDING_MAX: usize = 1024;
const PENDING_INPUT_MAX: usize = 256;
const SNAPSHOT_BYTES_MAX: usize = 4 * 1024 * 1024;

const _: () = assert!(
    PENDING_INPUT_MAX + 128 * 4 <= PENDING_MAX,
    "data admission must reserve four changes for each of 128 native lifecycles"
);

pub(super) struct Consumer {
    owner: uuid::Uuid,
    lifetime: Arc<std::sync::atomic::AtomicBool>,
    tracker: Tracker,
    sent: SyncSender<KeepPeekEvent>,
    received: Receiver<Input>,
    slot: Arc<Slot>,
    shutdown: Shutdown,
    pending: VecDeque<KeepPeekEvent>,
    acknowledgement: Option<(Receiver<usize>, usize)>,
    work: VecDeque<Work>,
    snapshots: Option<SyncSender<super::snapshot::Job>>,
    input_cutoff: Option<Instant>,
    metadata_cutoff: Option<Instant>,
}

enum Work {
    Notification(onvif::event::Notification, Instant, i64),
    MetadataNotification(onvif::event::Notification, Instant, i64),
    Frame(onvif::event::Frame, Instant, i64),
}

impl Consumer {
    pub fn new(
        camera: &Camera,
        sent: SyncSender<KeepPeekEvent>,
        received: Receiver<Input>,
        slot: Arc<Slot>,
        shutdown: Shutdown,
        snapshots: Option<SyncSender<super::snapshot::Job>>,
    ) -> Self {
        Self {
            owner: uuid::Uuid::new_v4(),
            lifetime: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            tracker: Tracker::new(
                camera.config.ip.to_string(),
                camera.config.events.clone(),
                camera.config.record_generic_motion_events,
            ),
            sent,
            received,
            slot,
            shutdown,
            pending: VecDeque::new(),
            acknowledgement: None,
            work: VecDeque::new(),
            snapshots,
            input_cutoff: None,
            metadata_cutoff: None,
        }
    }

    pub fn run(mut self) {
        let mut next_report = Instant::now() + Duration::from_secs(30);
        while !self.shutdown.is_cancelled() {
            if !self.advance() {
                break;
            }
            self.pending.extend(self.tracker.expire(Instant::now()));
            assert!(
                self.pending.len() <= PENDING_MAX,
                "native lifecycle pending reserve is sufficient"
            );
            if !self.flush() {
                break;
            }
            self.report(false);
            if Instant::now() >= next_report {
                self.report(true);
                next_report = Instant::now() + Duration::from_secs(30);
            }
        }
        self.finish();
    }

    fn advance(&mut self) -> bool {
        self.interruptions();
        if self.pending.len() >= PENDING_INPUT_MAX {
            self.shutdown.wait_timeout(Duration::from_millis(25));
        } else if let Some(work) = self.work.pop_front() {
            let changes = match work {
                Work::Notification(notification, received, received_ms) => {
                    self.tracker.apply(&notification, received, received_ms)
                }
                Work::MetadataNotification(notification, received, received_ms) => self
                    .tracker
                    .apply_metadata(&notification, received, received_ms),
                Work::Frame(frame, received, received_ms) => {
                    self.tracker.frame(&frame, received, received_ms)
                }
            };
            match changes {
                Ok(changes) => self.enqueue(changes),
                Err(_) => self.slot.update(|evidence| evidence.parse_errors += 1),
            }
        } else {
            match self.received.recv_timeout(Duration::from_millis(100)) {
                Ok(input) => {
                    self.slot.consumed(&input);
                    self.accept(input);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            }
        }
        true
    }

    fn finish(&mut self) {
        self.pending.extend(self.tracker.disconnect("shutdown"));
        self.pending
            .extend(self.tracker.expire(Instant::now() + Duration::from_secs(1)));
        assert!(
            self.pending.len() <= PENDING_MAX,
            "shutdown endings must fit the lifecycle reserve"
        );
        let until = Instant::now() + Duration::from_secs(5);
        while !self.pending.is_empty() && Instant::now() < until {
            if !self.flush() {
                break;
            }
            std::thread::park_timeout(Duration::from_millis(10));
        }
        self.slot.update(|evidence| {
            evidence.state = "stopped";
            evidence.dropped += self.pending.len() as u64;
        });
        self.report(true);
    }

    fn accept(&mut self, input: Input) {
        match input {
            Input::Snapshot {
                camera_id,
                event_id,
                jpeg,
            } => {
                let retained = self
                    .pending
                    .iter()
                    .map(|change| {
                        if let KeepPeekEvent::TimelineEventThumbnail { jpeg, .. } = change {
                            jpeg.len()
                        } else {
                            0
                        }
                    })
                    .sum::<usize>();
                if retained + jpeg.len() <= SNAPSHOT_BYTES_MAX {
                    self.pending
                        .push_back(KeepPeekEvent::TimelineEventThumbnail {
                            camera_id,
                            event_id,
                            jpeg,
                        });
                } else {
                    self.slot.update(|evidence| evidence.snapshot_failures += 1);
                }
            }
            Input::Disconnected => self
                .pending
                .extend(self.tracker.disconnect_pullpoint("transport_lost")),
            Input::MetadataLost => {
                self.interruptions();
            }
            Input::Metadata {
                bytes,
                compression,
                loss,
                received,
                received_ms,
            } => self.accept_metadata(&bytes, compression, loss, received, received_ms),
            Input::Pull {
                bytes,
                received,
                received_ms,
            } => self.accept_pull(&bytes, received, received_ms),
        }
    }

    fn accept_pull(&mut self, bytes: &[u8], received: Instant, received_ms: i64) {
        if self.input_cutoff.is_some_and(|cutoff| received <= cutoff) {
            self.slot.update(|evidence| evidence.queue_drops += 1);
            return;
        }
        let Some(received_time) = chrono::DateTime::from_timestamp_millis(received_ms) else {
            self.slot.update(|evidence| evidence.parse_errors += 1);
            return;
        };
        match onvif::event::Pull::parse_at(bytes, received_time) {
            Ok(pull) => {
                self.slot.update(|evidence| {
                    evidence.notifications += pull.notifications.len() as u64;
                    evidence.parse_errors += u64::from(pull.invalid_messages);
                });
                self.work.extend(
                    pull.notifications.into_iter().map(|notification| {
                        Work::Notification(notification, received, received_ms)
                    }),
                );
            }
            Err(_) => self.slot.update(|evidence| evidence.parse_errors += 1),
        }
    }

    fn accept_metadata(
        &mut self,
        bytes: &[u8],
        compression: retina::codec::CompressionType,
        loss: u16,
        received: Instant,
        received_ms: i64,
    ) {
        if self
            .metadata_cutoff
            .is_some_and(|cutoff| received <= cutoff)
        {
            self.slot.update(|evidence| evidence.queue_drops += 1);
            return;
        }
        if loss > 0 {
            self.pending
                .extend(self.tracker.disconnect_metadata("RTP_loss"));
        }
        self.slot.update(|evidence| {
            evidence.metadata_bytes += bytes.len() as u64;
            evidence.metadata_loss += u64::from(loss);
        });
        let Some(received_time) = chrono::DateTime::from_timestamp_millis(received_ms) else {
            self.slot.update(|evidence| evidence.metadata_errors += 1);
            return;
        };
        match super::metadata::decode(bytes, compression, received_time) {
            Ok(metadata) => {
                self.slot.update(|evidence| {
                    evidence.metadata_documents += 1;
                    evidence.parse_errors += u64::from(metadata.invalid_messages);
                });
                self.work
                    .extend(metadata.notifications.into_iter().map(|notification| {
                        Work::MetadataNotification(notification, received, received_ms)
                    }));
                self.work.extend(
                    metadata
                        .frames
                        .into_iter()
                        .map(|frame| Work::Frame(frame, received, received_ms)),
                );
            }
            Err(_) => self.slot.update(|evidence| evidence.metadata_errors += 1),
        }
    }

    fn interruptions(&mut self) {
        let (pull, metadata) = self.slot.take_interruptions();
        if let Some(cutoff) = pull
            && self.input_cutoff.is_none_or(|previous| cutoff > previous)
        {
            self.input_cutoff = Some(cutoff);
            let before = self.work.len();
            self.work.retain(
                |work| !matches!(work, Work::Notification(_, received, _) if *received <= cutoff),
            );
            self.slot
                .update(|evidence| evidence.queue_drops += (before - self.work.len()) as u64);
            self.pending
                .extend(self.tracker.disconnect_pullpoint("queue_continuity_lost"));
        }
        if let Some(cutoff) = metadata
            && self
                .metadata_cutoff
                .is_none_or(|previous| cutoff > previous)
        {
            self.metadata_cutoff = Some(cutoff);
            let before = self.work.len();
            self.work.retain(|work| !matches!(work, Work::MetadataNotification(_, received, _) | Work::Frame(_, received, _) if *received <= cutoff));
            self.slot
                .update(|evidence| evidence.queue_drops += (before - self.work.len()) as u64);
            self.pending
                .extend(self.tracker.disconnect_metadata("metadata_queue_lost"));
        }
    }

    fn enqueue(&mut self, changes: Vec<KeepPeekEvent>) {
        for change in &changes {
            if let KeepPeekEvent::TimelineEventStarted { event } = change {
                self.slot.update(|evidence| {
                    if !evidence.kinds.contains(&event.kind) {
                        evidence.kinds.push(event.kind.clone());
                    }
                });
                if let Some(snapshots) = &self.snapshots
                    && snapshots
                        .try_send(super::snapshot::Job {
                            camera_id: event.camera_id.clone(),
                            event_id: event.id.clone(),
                        })
                        .is_err()
                {
                    self.slot.update(|evidence| evidence.snapshot_failures += 1);
                }
            }
        }
        assert!(
            self.pending.len() + changes.len() <= PENDING_MAX,
            "bounded native event batch must fit reserved pending capacity"
        );
        self.pending.extend(changes);
    }

    fn flush(&mut self) -> bool {
        if let Some((reply, offered)) = &self.acknowledgement {
            match reply.try_recv() {
                Ok(committed) => {
                    assert!(
                        committed <= *offered,
                        "native commit prefix cannot exceed offered changes"
                    );
                    self.pending.drain(..committed);
                    self.acknowledgement = None;
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.slot
                        .update(|evidence| evidence.state = "commit_unknown");
                    return false;
                }
            }
        }
        if self.pending.is_empty() {
            return true;
        }
        let changes = self.pending.iter().take(128).cloned().collect::<Vec<_>>();
        let offered = changes.len();
        let (reply, received) = mpsc::sync_channel(1);
        match self.sent.try_send(KeepPeekEvent::NativeBatch {
            owner: self.owner,
            lifetime: Arc::clone(&self.lifetime),
            changes,
            reply,
        }) {
            Ok(()) => self.acknowledgement = Some((received, offered)),
            Err(mpsc::TrySendError::Full(_)) => {
                self.slot.update(|evidence| evidence.delivery_stalls += 1);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
        true
    }

    fn report(&self, log: bool) {
        self.slot.update(|evidence| {
            evidence.active = self.tracker.active_count();
            evidence.deduplicated = self.tracker.deduplicated();
            for kind in self.tracker.kinds() { if !evidence.kinds.contains(&kind) { evidence.kinds.push(kind); } }
            if log { tracing::info!(name: "camera.events.status", mode = evidence.mode, state = evidence.state, pulls = evidence.pulls, notifications = evidence.notifications, active = evidence.active, parse_errors = evidence.parse_errors, deduplicated = evidence.deduplicated, pending = self.pending.len(), "native camera event status"); }
        });
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        self.lifetime
            .store(false, std::sync::atomic::Ordering::Release);
        self.shutdown.cancel();
    }
}

#[cfg(test)]
mod continuity_tests;

#[cfg(test)]
mod pressure_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cameras::events::EventConfig;

    const RECEIVED_MS: i64 = 1_788_609_605_123;

    fn timestamp_message(attributes: &str) -> String {
        format!(
            r#"<n:NotificationMessage xmlns:n="http://docs.oasis-open.org/wsn/b-2"
                xmlns:t="http://www.onvif.org/ver10/schema"
                xmlns:q="http://www.onvif.org/ver10/topics">
                <n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">q:VideoSource/MotionAlarm</n:Topic>
                <n:Message><t:Message {attributes} PropertyOperation="Changed">
                    <t:Data><t:SimpleItem Name="State" Value="true"/></t:Data>
                </t:Message></n:Message>
            </n:NotificationMessage>"#
        )
    }

    fn assert_received(consumer: &mut Consumer, received: Instant, reason: &str) {
        assert_eq!(consumer.work.len(), 1);
        match &consumer.work[0] {
            Work::Notification(notification, queued, queued_ms)
            | Work::MetadataNotification(notification, queued, queued_ms) => {
                assert_eq!(*queued, received);
                assert_eq!(*queued_ms, RECEIVED_MS);
                assert_eq!(notification.utc_time.timestamp_millis(), RECEIVED_MS);
            }
            Work::Frame(..) => panic!("expected a notification"),
        }
        assert!(consumer.advance());
        let Some(KeepPeekEvent::TimelineEventStarted { event }) = consumer.pending.front() else {
            panic!("timestamp fallback must start a native detection");
        };
        assert_eq!(event.start_time_ms, RECEIVED_MS);
        let payload = event.payload.as_ref().unwrap();
        assert_eq!(payload["timestamp_source"], "received");
        assert_eq!(payload["timestamp_reason"], reason);
        assert!(payload["cameraTime"].is_null());
        assert!(
            !serde_json::to_string(payload)
                .unwrap()
                .contains("private-invalid-clock")
        );
        let counts = {
            let evidence = consumer.slot.evidence.lock().unwrap();
            (evidence.parse_errors, evidence.metadata_errors)
        };
        assert_eq!(counts, (0, 0));
    }

    #[test]
    fn pull_timestamp_fallback_uses_input_receipt_without_parse_errors() {
        for (attributes, reason) in [
            ("", "camera_time_missing"),
            (r#"UtcTime="private-invalid-clock""#, "camera_time_invalid"),
        ] {
            let (mut consumer, _output) = continuity_tests::consumer();
            let received = Instant::now();
            let message = timestamp_message(attributes);
            let bytes = format!(
                r#"<e:PullMessagesResponse xmlns:e="http://www.onvif.org/ver10/events/wsdl">
                    <e:CurrentTime>2026-09-05T12:00:00Z</e:CurrentTime>
                    <e:TerminationTime>2026-09-05T12:01:30Z</e:TerminationTime>
                    {message}
                </e:PullMessagesResponse>"#
            )
            .into_bytes();
            consumer.accept(Input::Pull {
                bytes,
                received,
                received_ms: RECEIVED_MS,
            });
            assert_received(&mut consumer, received, reason);
            let count = consumer.slot.evidence.lock().unwrap().notifications;
            assert_eq!(count, 1);
        }
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn metadata_timestamp_fallback_uses_input_receipt_for_plain_and_gzip_xml() {
        use retina::codec::CompressionType;

        for (attributes, reason) in [
            ("", "camera_time_missing"),
            (r#"UtcTime="private-invalid-clock""#, "camera_time_invalid"),
        ] {
            let message = timestamp_message(attributes);
            let xml = format!(
                r#"<t:MetadataStream xmlns:t="http://www.onvif.org/ver10/schema">
                    <t:Event>{message}</t:Event>
                </t:MetadataStream>"#
            );
            for (compression, bytes) in [
                (CompressionType::Uncompressed, xml.as_bytes().to_vec()),
                (CompressionType::GzipCompressed, gzip(xml.as_bytes())),
            ] {
                let (mut consumer, _output) = continuity_tests::consumer();
                let received = Instant::now();
                consumer.accept(Input::Metadata {
                    bytes,
                    compression,
                    loss: 0,
                    received,
                    received_ms: RECEIVED_MS,
                });
                assert_received(&mut consumer, received, reason);
                let count = consumer.slot.evidence.lock().unwrap().metadata_documents;
                assert_eq!(count, 1);
            }
        }
    }

    #[test]
    fn disconnect_is_latched_when_the_native_input_queue_is_full() {
        let registry = crate::camera_events::Registry::default();
        let shutdown = Shutdown::new();
        let (sent, received) = mpsc::sync_channel(1);
        let slot = registry
            .install(
                "127.0.0.1".parse().unwrap(),
                sent,
                shutdown,
                EventConfig::default(),
            )
            .unwrap();
        assert!(slot.try_send(Input::MetadataLost).is_ok());
        assert!(slot.try_send(Input::Disconnected).is_err());
        assert!(slot.take_interruptions().0.is_some());
        assert_eq!(slot.take_interruptions(), (None, None));
        assert!(matches!(received.recv().unwrap(), Input::MetadataLost));
    }
}
