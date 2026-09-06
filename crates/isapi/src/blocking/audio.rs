use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{Client, Control, REQUEST_TIMEOUT, STREAM_LIFETIME_MAX, make_agent};
use crate::Error;
use crate::error::Kind;
use crate::management::{AudioChannel, AudioSession, ResponseStatus};

const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

mod capture;
mod microphone;
mod speaker;
mod tcp;
pub use microphone::Microphone;
pub use speaker::Speaker;

/// An explicitly opened two-way audio session owned by this client.
///
/// Call [`Self::close`] to observe cleanup errors. Dropping this value attempts
/// one close within two seconds. It does not retry an unknown close outcome.
/// ISAPI closes a channel, not a session ID. The caller must coordinate exclusive
/// channel use with other applications. Use [`Self::abandon`] if ownership is lost.
pub struct Talk {
    client: Client,
    channel: AudioChannel,
    session: AudioSession,
    closed: bool,
    stopped: Arc<AtomicBool>,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    deadline: Instant,
    speaker_opened: bool,
    microphone_opened: bool,
}

impl Client {
    /// Opens an enabled audio channel with a bounded session lifetime.
    ///
    /// Supports raw 8 kHz mono G.711 A-law and mu-law in either direction.
    /// This consumes the client, never enables a channel, and never closes a busy
    /// session owned by another client. An unconfirmed open outcome is not retried.
    ///
    /// # Errors
    /// Rejects invalid lifetimes, unsupported codecs, disabled channels, mismatched
    /// channel IDs, authentication failures and device or network failures.
    pub fn open_audio(mut self, channel: u32, lifetime: Duration) -> Result<Talk, Error> {
        if lifetime.is_zero() || lifetime > STREAM_LIFETIME_MAX {
            return Err(Error::new(Kind::InvalidInput));
        }
        let deadline = Instant::now() + lifetime;
        let setup = deadline.min(Instant::now() + REQUEST_TIMEOUT);
        let configuration = self.query_until(&AudioChannel::query(channel)?, setup)?;
        if configuration.id() != channel {
            return Err(Error::new(Kind::Protocol));
        }
        let speaker = configuration.speaker_supported()
            && configuration.output_codec().sample_rate_hz().is_some();
        let microphone = configuration.microphone_supported()
            && configuration.input_codec().sample_rate_hz().is_some();
        if !configuration.enabled() || (!speaker && !microphone) {
            return Err(Error::new(Kind::InvalidInput));
        }
        let session = self.query_until(&configuration.open()?, setup)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let user_cancelled = Arc::clone(&self.cancelled);
        let session_stopped = Arc::clone(&stopped);
        let cancelled =
            Arc::new(move || user_cancelled() || session_stopped.load(Ordering::Acquire));
        self.cancelled = Arc::new(|| false);
        self.agent = make_agent(Control::new(Arc::clone(&self.cancelled), None));
        Ok(Talk {
            client: self,
            channel: configuration,
            session,
            closed: false,
            stopped,
            cancelled,
            deadline,
            speaker_opened: false,
            microphone_opened: false,
        })
    }
}

impl Talk {
    /// Returns the verified channel configuration without exposing credentials.
    pub const fn channel(&self) -> &AudioChannel {
        &self.channel
    }

    /// Returns the session identity for Sans-I/O request construction.
    pub const fn session(&self) -> &AudioSession {
        &self.session
    }

    /// Reports cancellation or expiration without performing network I/O.
    pub fn is_stopped(&self) -> bool {
        (self.cancelled)() || Instant::now() >= self.deadline
    }

    /// Stops local audio I/O and sends one explicit channel-close request.
    ///
    /// # Errors
    /// Returns a device, authentication or network error. A failed close is not
    /// replayed by `Drop`, because the camera's outcome may be unknown.
    pub fn close(mut self) -> Result<ResponseStatus, Error> {
        self.release()
    }

    /// Cancels local handles without closing the device channel.
    ///
    /// Use this when another application has replaced the session. The protocol
    /// has no session-scoped close, so automatic cleanup cannot detect that change.
    pub fn abandon(mut self) {
        self.closed = true;
        self.stopped.store(true, Ordering::Release);
    }

    fn release(&mut self) -> Result<ResponseStatus, Error> {
        self.closed = true;
        self.stopped.store(true, Ordering::Release);
        self.client
            .command_until(&self.channel.close()?, Instant::now() + CLOSE_TIMEOUT)
    }
}

impl Drop for Talk {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.release();
        }
    }
}

impl fmt::Debug for Talk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Talk")
            .field("channel", &self.channel.id())
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}
