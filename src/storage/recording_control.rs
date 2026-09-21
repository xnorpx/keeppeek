//! Resolves temporary recording requests inside configured and privacy bounds.
//!
//! Callers serialize this state with media admission and authenticate each mutation.
//! Requests are ephemeral. A new instance invalidates every previous revision token.

use crate::cameras::CameraRecordingMode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A wall-clock observation and its monotonic companion.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub monotonic: Instant,
    pub utc_ms: Option<i64>,
}

impl Clock {
    pub fn now() -> Self {
        Self {
            monotonic: Instant::now(),
            utc_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_millis()).ok()),
        }
    }
}

/// A process-local compare-and-swap token, including an instance epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revision {
    pub epoch: u128,
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Manual,
    External,
}

/// A bounded request. The actor comes from the authenticated server principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    pub enabled: bool,
    pub source: Source,
    pub actor: String,
    pub reason: String,
    pub ttl_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Configuration,
    ConfiguredDisabled,
    Privacy,
    PrivacyUnavailable,
    Override,
    Expired,
    ClockUnavailable,
}

/// The current effective permission; stream and event selection remain separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub revision: Revision,
    pub configured_mode: CameraRecordingMode,
    pub mode: CameraRecordingMode,
    pub reason: Reason,
    pub request: Option<Override>,
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone)]
struct Pending {
    request: Override,
    expires_at_ms: i64,
    expires_at: Instant,
    created_at: Instant,
    invalid_clock: bool,
}

/// One camera's authoritative permission state. No request changes configured mode.
#[derive(Debug, Clone)]
pub struct Control {
    configured_mode: CameraRecordingMode,
    privacy: Option<bool>,
    revision: Revision,
    pending: Option<Pending>,
    expired: bool,
}

impl Control {
    pub fn new(configured_mode: CameraRecordingMode) -> Self {
        Self {
            configured_mode,
            privacy: Some(false),
            revision: Revision {
                epoch: rand::random(),
                sequence: 0,
            },
            pending: None,
            expired: false,
        }
    }

    pub const fn revision(&self) -> Revision {
        self.revision
    }

    pub const fn configured_mode(&self) -> CameraRecordingMode {
        self.configured_mode
    }

    fn advance(&mut self) {
        self.revision = match self.revision.sequence.checked_add(1) {
            Some(sequence) => Revision {
                sequence,
                ..self.revision
            },
            None => Revision {
                epoch: rand::random(),
                sequence: 0,
            },
        };
    }

    /// Unknown required privacy state fails closed.
    pub fn set_privacy(&mut self, active: Option<bool>) {
        if self.privacy != active {
            self.privacy = active;
            self.advance();
        }
    }

    /// Reconfiguration invalidates stale callers but preserves the privacy bound.
    pub fn configure(&mut self, mode: CameraRecordingMode) {
        self.configured_mode = mode;
        self.pending = None;
        self.expired = false;
        self.advance();
    }

    pub fn set_override(
        &mut self,
        expected: Revision,
        request: Override,
        now: Clock,
    ) -> anyhow::Result<()> {
        self.effective(now);
        anyhow::ensure!(
            expected == self.revision,
            "recording control revision conflict"
        );
        anyhow::ensure!(
            (1..=86_400_000).contains(&request.ttl_ms),
            "recording override TTL must be 1..86400000 ms"
        );
        anyhow::ensure!(
            !request.actor.trim().is_empty() && request.actor.len() <= 128,
            "invalid recording override actor"
        );
        anyhow::ensure!(
            !request.reason.trim().is_empty() && request.reason.len() <= 256,
            "invalid recording override reason"
        );
        if request.enabled {
            anyhow::ensure!(
                self.configured_mode != CameraRecordingMode::Off,
                "recording is configured off"
            );
            anyhow::ensure!(self.privacy == Some(false), "privacy prevents recording");
        }
        let utc_ms = now
            .utc_ms
            .ok_or_else(|| anyhow::anyhow!("recording clock is unavailable"))?;
        let expires_at_ms = utc_ms
            .checked_add(i64::try_from(request.ttl_ms)?)
            .ok_or_else(|| anyhow::anyhow!("recording override expiry overflow"))?;
        let expires_at = now
            .monotonic
            .checked_add(Duration::from_millis(request.ttl_ms))
            .ok_or_else(|| anyhow::anyhow!("recording override expiry overflow"))?;
        self.pending = Some(Pending {
            request,
            expires_at_ms,
            expires_at,
            created_at: now.monotonic,
            invalid_clock: false,
        });
        self.expired = false;
        self.advance();
        Ok(())
    }

    pub fn clear_override(&mut self, expected: Revision, now: Clock) -> anyhow::Result<()> {
        self.effective(now);
        anyhow::ensure!(
            expected == self.revision,
            "recording control revision conflict"
        );
        self.pending = None;
        self.expired = false;
        self.advance();
        Ok(())
    }

    /// Resolves permission without allocating in the frame-admission path.
    pub fn effective(&mut self, now: Clock) -> (CameraRecordingMode, Reason) {
        if self.pending.as_ref().is_some_and(|pending| {
            !pending.invalid_clock && (now.utc_ms.is_none() || now.monotonic < pending.created_at)
        }) {
            self.pending
                .as_mut()
                .expect("pending override was checked")
                .invalid_clock = true;
            self.advance();
        }
        if self.pending.as_ref().is_some_and(|pending| {
            now.monotonic >= pending.expires_at
                || now
                    .utc_ms
                    .is_some_and(|utc_ms| utc_ms >= pending.expires_at_ms)
        }) {
            self.pending = None;
            self.expired = true;
            self.advance();
        }
        let off = CameraRecordingMode::Off;
        if self.configured_mode == off {
            return (off, Reason::ConfiguredDisabled);
        }
        match self.privacy {
            None => return (off, Reason::PrivacyUnavailable),
            Some(true) => return (off, Reason::Privacy),
            Some(false) => {}
        }
        if let Some(pending) = &self.pending {
            if pending.invalid_clock {
                return (off, Reason::ClockUnavailable);
            }
            return (
                if pending.request.enabled {
                    self.configured_mode
                } else {
                    off
                },
                Reason::Override,
            );
        }
        (
            self.configured_mode,
            if self.expired {
                Reason::Expired
            } else {
                Reason::Configuration
            },
        )
    }

    pub fn snapshot(&mut self, now: Clock) -> Snapshot {
        let (mode, reason) = self.effective(now);
        Snapshot {
            revision: self.revision,
            configured_mode: self.configured_mode,
            mode,
            reason,
            request: self.pending.as_ref().map(|pending| pending.request.clone()),
            expires_at_ms: self.pending.as_ref().map(|pending| pending.expires_at_ms),
        }
    }
}
