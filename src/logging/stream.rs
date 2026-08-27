use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;

use super::hub::{LogEntry, LogSubscription};

pub struct LogStreamReader {
    subscription: LogSubscription,
    heartbeat: Duration,
    initial_frame_pending: bool,
    pending: Vec<u8>,
    pending_offset: usize,
    cancellation: Option<Arc<AtomicBool>>,
}

#[derive(Serialize)]
struct GapEvent {
    dropped: u64,
}

impl LogStreamReader {
    pub(super) const fn new(subscription: LogSubscription, heartbeat: Duration) -> Self {
        Self {
            subscription,
            heartbeat,
            initial_frame_pending: true,
            pending: Vec::new(),
            pending_offset: 0,
            cancellation: None,
        }
    }

    pub(crate) fn with_cancellation(mut self, cancellation: Arc<AtomicBool>) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    fn next_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        if std::mem::take(&mut self.initial_frame_pending) {
            return Ok(Some(b": connected\n\n".to_vec()));
        }
        if self.subscription.take_replay_truncated() {
            return Ok(Some(b"event: replay-truncated\ndata: {}\n\n".to_vec()));
        }
        if let Some(entry) = self.subscription.pop_replay() {
            return encode_log_entry(&entry).map(Some);
        }
        match self.subscription.try_next() {
            Ok(entry) => return encode_log_entry(&entry).map(Some),
            Err(crossbeam_channel::TryRecvError::Disconnected) => return Ok(None),
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }
        let dropped = self.subscription.take_dropped();
        if dropped > 0 {
            return serde_json::to_string(&GapEvent { dropped })
                .map(|data| Some(format!("event: gap\ndata: {data}\n\n").into_bytes()))
                .map_err(io::Error::other);
        }
        let started_at = Instant::now();
        loop {
            if self
                .cancellation
                .as_ref()
                .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
            {
                return Ok(None);
            }
            let remaining = self
                .heartbeat
                .saturating_sub(started_at.elapsed())
                .min(Duration::from_millis(100));
            match self.subscription.next_timeout(remaining) {
                Ok(entry) => return encode_log_entry(&entry).map(Some),
                Err(crossbeam_channel::RecvTimeoutError::Timeout)
                    if started_at.elapsed() < self.heartbeat => {}
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    return Ok(Some(b": keep-alive\n\n".to_vec()));
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }
}

impl io::Read for LogStreamReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.pending_offset >= self.pending.len() {
            let Some(frame) = self.next_frame()? else {
                return Ok(0);
            };
            self.pending = frame;
            self.pending_offset = 0;
        }

        let remaining = &self.pending[self.pending_offset..];
        let read = remaining.len().min(buffer.len());
        buffer[..read].copy_from_slice(&remaining[..read]);
        self.pending_offset += read;
        Ok(read)
    }
}

fn encode_log_entry(entry: &LogEntry) -> io::Result<Vec<u8>> {
    let data = serde_json::to_string(entry).map_err(io::Error::other)?;
    Ok(format!("id: {}\nevent: log\ndata: {data}\n\n", entry.sequence).into_bytes())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, io::Read};

    use super::*;
    use crate::logging::{LogHub, LogLevel};

    #[test]
    fn frames_replayed_entries_as_sse() {
        let hub = LogHub::default();
        hub.record(
            LogLevel::Info,
            "keeppeek::test",
            "ready".to_owned(),
            BTreeMap::new(),
            Some("src/test.rs"),
            Some(42),
        );
        let subscription = hub.subscribe(None, 10).unwrap();
        hub.close();
        let mut reader = LogStreamReader::new(subscription, Duration::from_secs(1));
        let mut output = String::new();

        reader.read_to_string(&mut output).unwrap();

        assert!(output.starts_with(": connected\n\n"));
        assert!(output.contains("id: 1\nevent: log\ndata: "));
        assert!(output.contains("\"message\":\"ready\""));
        assert!(output.ends_with("\n\n"));
    }

    #[test]
    fn emits_heartbeat_while_stream_is_idle() {
        let hub = LogHub::default();
        let subscription = hub.subscribe(None, 10).unwrap();
        let mut reader = LogStreamReader::new(subscription, Duration::from_millis(1));
        let mut connected = [0; 13];
        let mut heartbeat = [0; 14];

        reader.read_exact(&mut connected).unwrap();
        reader.read_exact(&mut heartbeat).unwrap();

        assert_eq!(&connected, b": connected\n\n");
        assert_eq!(&heartbeat, b": keep-alive\n\n");
    }

    #[test]
    fn cancellation_ends_an_authenticated_stream() {
        let hub = LogHub::default();
        let subscription = hub.subscribe(None, 10).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut reader = LogStreamReader::new(subscription, Duration::from_secs(1))
            .with_cancellation(cancelled.clone());
        let mut connected = [0; 13];
        reader.read_exact(&mut connected).unwrap();
        assert_eq!(&connected, b": connected\n\n");

        cancelled.store(true, Ordering::Release);
        let mut byte = [0; 1];
        assert_eq!(reader.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn reports_entries_dropped_for_a_slow_reader() {
        let hub = LogHub::new(crate::logging::LogHubLimits {
            subscriber_capacity: 1,
            ..crate::logging::LogHubLimits::default()
        });
        let subscription = hub.subscribe(None, 10).unwrap();
        hub.record(
            LogLevel::Info,
            "keeppeek::test",
            "queued".to_owned(),
            BTreeMap::new(),
            None,
            None,
        );
        hub.record(
            LogLevel::Info,
            "keeppeek::test",
            "dropped".to_owned(),
            BTreeMap::new(),
            None,
            None,
        );
        let mut reader = LogStreamReader::new(subscription, Duration::from_secs(1));
        let mut output = vec![0; 512];

        let first_read = reader.read(&mut output).unwrap();
        let second_read = reader.read(&mut output[first_read..]).unwrap();
        let third_read = reader
            .read(&mut output[first_read + second_read..])
            .unwrap();
        let output =
            String::from_utf8(output[..first_read + second_read + third_read].to_vec()).unwrap();

        assert!(output.contains("\"message\":\"queued\""));
        assert!(output.contains("event: gap\ndata: {\"dropped\":1}"));
    }
}
