//! Normalizes one event revision for an IP camera without asserting producer completeness.

use super::Settings;
use crate::storage::{
    metadata::TimelineEvent,
    retention::{Evidence, Interval},
};
use std::net::IpAddr;

/// Raw event identity remains available even when a revision no longer supplies usable evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventEvidence<'a> {
    pub event_id: &'a str,
    pub revision: u64,
    pub outcome: EventOutcome,
}

/// Available means one closed interval, never a complete producer history or deletion permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventOutcome {
    NotApplicable,
    Unavailable(UnavailableReason),
    Available(Evidence),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    InvalidIdentity,
    UnknownKind,
    OpenInterval,
    InvalidInterval,
}

impl Settings {
    /// Normalizes canonical IP-camera events for one logical main or sub stream.
    /// Opaque catalog source IDs require a separate adapter; no detector payload is inferred.
    ///
    /// # Errors
    /// Rejects target streams other than `main` or `sub`.
    pub fn normalize_event<'a>(
        &self,
        camera: IpAddr,
        stream: &str,
        event: &'a TimelineEvent,
    ) -> anyhow::Result<EventEvidence<'a>> {
        anyhow::ensure!(
            matches!(stream, "main" | "sub"),
            "invalid retention target stream"
        );
        Ok(EventEvidence {
            event_id: &event.id,
            revision: event.revision,
            outcome: self.event_outcome(camera, stream, event),
        })
    }

    fn event_outcome(&self, camera: IpAddr, stream: &str, event: &TimelineEvent) -> EventOutcome {
        use EventOutcome::{Available, NotApplicable, Unavailable};
        use UnavailableReason::{InvalidIdentity, InvalidInterval, OpenInterval, UnknownKind};
        let event_camera = event.camera_id.parse::<IpAddr>();
        let Ok(event_camera) = event_camera else {
            return Unavailable(InvalidIdentity);
        };
        if event_camera.to_string() != event.camera_id {
            return Unavailable(InvalidIdentity);
        }
        if event_camera != camera {
            return NotApplicable;
        }
        if event.id.is_empty() || event.revision == 0 || i64::try_from(event.revision).is_err() {
            return Unavailable(InvalidIdentity);
        }
        match event.stream.as_deref() {
            Some("main" | "sub") if event.stream.as_deref() != Some(stream) => {
                return NotApplicable;
            }
            Some("main" | "sub") | None => {}
            Some(_) => return Unavailable(InvalidIdentity),
        }
        let Some(kind) = self.classify(camera, event.source, &event.kind) else {
            return Unavailable(UnknownKind);
        };
        let Some(end_ms) = event.end_time_ms else {
            return Unavailable(OpenInterval);
        };
        match Interval::new(event.start_time_ms, end_ms) {
            Ok(interval) => Available(Evidence::new(kind, interval)),
            Err(_) => Unavailable(InvalidInterval),
        }
    }
}
