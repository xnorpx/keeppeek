use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_REPLAY_ENTRIES: usize = 1_000;
const MAX_ENTRY_BYTES: usize = 64 * 1024;
const MAX_MESSAGE_CHARS: usize = 16 * 1024;
const MAX_FIELD_CHARS: usize = 8 * 1024;
const MAX_FIELDS: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<&tracing::Level> for LogLevel {
    fn from(level: &tracing::Level) -> Self {
        match *level {
            tracing::Level::TRACE => Self::Trace,
            tracing::Level::DEBUG => Self::Debug,
            tracing::Level::INFO => Self::Info,
            tracing::Level::WARN => Self::Warn,
            tracing::Level::ERROR => Self::Error,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LogEntry {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
    pub fields: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LogBufferStats {
    pub entry_count: usize,
    pub byte_count: usize,
    pub evicted_entries: u64,
    pub max_entries: usize,
    pub max_bytes: usize,
    pub active_streams: usize,
    pub max_streams: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LogSnapshot {
    pub entries: Vec<LogEntry>,
    pub oldest_sequence: Option<u64>,
    pub newest_sequence: Option<u64>,
    pub truncated: bool,
    pub stats: LogBufferStats,
}

#[derive(Clone, Copy, Debug)]
pub struct LogHubLimits {
    pub max_entries: usize,
    pub max_bytes: usize,
    pub subscriber_capacity: usize,
    pub max_subscribers: usize,
}

impl Default for LogHubLimits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_bytes: 8 * 1024 * 1024,
            subscriber_capacity: 512,
            max_subscribers: 8,
        }
    }
}

#[derive(Clone)]
pub struct LogHub {
    inner: Arc<Mutex<HubState>>,
    limits: LogHubLimits,
}

struct HubState {
    entries: VecDeque<BufferedEntry>,
    byte_count: usize,
    evicted_entries: u64,
    next_sequence: u64,
    next_subscriber_id: u64,
    subscribers: HashMap<u64, Subscriber>,
    closed: bool,
}

struct BufferedEntry {
    entry: LogEntry,
    byte_count: usize,
}

struct Subscriber {
    sender: Sender<LogEntry>,
    dropped: Arc<AtomicU64>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum LogStreamError {
    Closed,
    LimitReached,
}

impl fmt::Display for LogStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("log stream is shutting down"),
            Self::LimitReached => formatter.write_str("too many active log streams"),
        }
    }
}

impl std::error::Error for LogStreamError {}

pub(super) struct LogSubscription {
    id: u64,
    hub: Weak<Mutex<HubState>>,
    replay: VecDeque<LogEntry>,
    replay_truncated: bool,
    receiver: Receiver<LogEntry>,
    dropped: Arc<AtomicU64>,
}

impl LogHub {
    pub fn new(limits: LogHubLimits) -> Self {
        let limits = LogHubLimits {
            max_entries: limits.max_entries.max(1),
            max_bytes: limits.max_bytes.max(1),
            subscriber_capacity: limits.subscriber_capacity.max(1),
            max_subscribers: limits.max_subscribers.max(1),
        };
        Self {
            inner: Arc::new(Mutex::new(HubState {
                entries: VecDeque::new(),
                byte_count: 0,
                evicted_entries: 0,
                next_sequence: 1,
                next_subscriber_id: 1,
                subscribers: HashMap::new(),
                closed: false,
            })),
            limits,
        }
    }

    pub fn record(
        &self,
        level: LogLevel,
        target: &str,
        message: String,
        fields: BTreeMap<String, Value>,
        file: Option<&str>,
        line: Option<u32>,
    ) -> LogEntry {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);

        let mut entry = LogEntry {
            sequence,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            level,
            target: target.to_owned(),
            message: redact_text(&truncate_chars(&message, MAX_MESSAGE_CHARS)),
            fields: sanitize_fields(fields),
            file: file.map(ToOwned::to_owned),
            line,
        };
        let mut byte_count = serialized_size(&entry);
        if byte_count > MAX_ENTRY_BYTES {
            entry.fields.clear();
            entry
                .fields
                .insert("truncated".to_owned(), Value::Bool(true));
            entry.message = truncate_chars(&entry.message, MAX_FIELD_CHARS);
            byte_count = serialized_size(&entry);
        }

        state.byte_count = state.byte_count.saturating_add(byte_count);
        state.entries.push_back(BufferedEntry {
            entry: entry.clone(),
            byte_count,
        });
        while state.entries.len() > self.limits.max_entries
            || state.byte_count > self.limits.max_bytes
        {
            let Some(removed) = state.entries.pop_front() else {
                break;
            };
            state.byte_count = state.byte_count.saturating_sub(removed.byte_count);
            state.evicted_entries = state.evicted_entries.saturating_add(1);
        }

        state.subscribers.retain(
            |_, subscriber| match subscriber.sender.try_send(entry.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    subscriber.dropped.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            },
        );
        entry
    }

    pub fn snapshot(&self, after: Option<u64>, limit: usize) -> LogSnapshot {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let limit = limit.clamp(1, self.limits.max_entries);
        let oldest_sequence = state.entries.front().map(|entry| entry.entry.sequence);
        let newest_sequence = state.entries.back().map(|entry| entry.entry.sequence);
        let mut matching = state
            .entries
            .iter()
            .filter(|entry| after.is_none_or(|after| entry.entry.sequence > after))
            .map(|entry| entry.entry.clone())
            .collect::<Vec<_>>();
        let missing_before_buffer = after.is_some_and(|after| {
            oldest_sequence.is_some_and(|oldest| after.saturating_add(1) < oldest)
        });
        let over_limit = matching.len() > limit;
        if after.is_some() {
            matching.truncate(limit);
        } else if over_limit {
            matching.drain(..matching.len() - limit);
        }

        LogSnapshot {
            entries: matching,
            oldest_sequence,
            newest_sequence,
            truncated: state.evicted_entries > 0 || missing_before_buffer || over_limit,
            stats: self.stats_from_state(&state),
        }
    }

    pub fn stats(&self) -> LogBufferStats {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.stats_from_state(&state)
    }

    pub fn close(&self) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.subscribers.clear();
    }

    pub(super) fn subscribe(
        &self,
        after: Option<u64>,
        tail: usize,
    ) -> Result<LogSubscription, LogStreamError> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(LogStreamError::Closed);
        }
        if state.subscribers.len() >= self.limits.max_subscribers {
            return Err(LogStreamError::LimitReached);
        }

        let oldest_sequence = state.entries.front().map(|entry| entry.entry.sequence);
        let mut replay = state
            .entries
            .iter()
            .filter(|entry| after.is_none_or(|after| entry.entry.sequence > after))
            .map(|entry| entry.entry.clone())
            .collect::<Vec<_>>();
        let requested_tail = tail.clamp(1, MAX_REPLAY_ENTRIES);
        let replay_limit = if after.is_some() {
            MAX_REPLAY_ENTRIES
        } else {
            requested_tail
        };
        let missing_before_buffer = after.is_some_and(|after| {
            oldest_sequence.is_some_and(|oldest| after.saturating_add(1) < oldest)
        });
        let over_limit = replay.len() > replay_limit;
        if over_limit {
            replay.drain(..replay.len() - replay_limit);
        }

        let id = state.next_subscriber_id;
        state.next_subscriber_id = state.next_subscriber_id.saturating_add(1);
        let (sender, receiver) = bounded(self.limits.subscriber_capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        state.subscribers.insert(
            id,
            Subscriber {
                sender,
                dropped: dropped.clone(),
            },
        );

        Ok(LogSubscription {
            id,
            hub: Arc::downgrade(&self.inner),
            replay: replay.into(),
            replay_truncated: missing_before_buffer || over_limit,
            receiver,
            dropped,
        })
    }

    fn stats_from_state(&self, state: &HubState) -> LogBufferStats {
        LogBufferStats {
            entry_count: state.entries.len(),
            byte_count: state.byte_count,
            evicted_entries: state.evicted_entries,
            max_entries: self.limits.max_entries,
            max_bytes: self.limits.max_bytes,
            active_streams: state.subscribers.len(),
            max_streams: self.limits.max_subscribers,
        }
    }
}

impl Default for LogHub {
    fn default() -> Self {
        Self::new(LogHubLimits::default())
    }
}

impl LogSubscription {
    pub(super) fn pop_replay(&mut self) -> Option<LogEntry> {
        self.replay.pop_front()
    }

    pub(super) fn take_replay_truncated(&mut self) -> bool {
        std::mem::take(&mut self.replay_truncated)
    }

    pub(super) fn try_next(&self) -> Result<LogEntry, TryRecvError> {
        self.receiver.try_recv()
    }

    pub(super) fn next_timeout(
        &self,
        timeout: Duration,
    ) -> Result<LogEntry, crossbeam_channel::RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    pub(super) fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, Ordering::Relaxed)
    }
}

impl Drop for LogSubscription {
    fn drop(&mut self) {
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        hub.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .subscribers
            .remove(&self.id);
    }
}

fn serialized_size(entry: &LogEntry) -> usize {
    serde_json::to_vec(entry)
        .map(|serialized| serialized.len())
        .unwrap_or(MAX_ENTRY_BYTES)
}

fn sanitize_fields(fields: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    fields
        .into_iter()
        .take(MAX_FIELDS)
        .map(|(name, value)| {
            let value = if is_sensitive_name(&name) {
                Value::String("[REDACTED]".to_owned())
            } else {
                sanitize_value(value)
            };
            (name, value)
        })
        .collect()
}

fn sanitize_value(value: Value) -> Value {
    match value {
        Value::String(value) => {
            Value::String(redact_text(&truncate_chars(&value, MAX_FIELD_CHARS)))
        }
        Value::Array(values) => Value::Array(values.into_iter().map(sanitize_value).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(name, value)| {
                    let value = if is_sensitive_name(&name) {
                        Value::String("[REDACTED]".to_owned())
                    } else {
                        sanitize_value(value)
                    };
                    (name, value)
                })
                .collect(),
        ),
        value => value,
    }
}

fn is_sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "authorization",
        "credential",
        "api_key",
        "apikey",
        "cookie",
    ]
    .iter()
    .any(|sensitive| name.contains(sensitive))
}

fn redact_text(text: &str) -> String {
    let mut redacted = redact_url_userinfo(text);
    for marker in [
        "password=",
        "passwd=",
        "secret=",
        "token=",
        "authorization=",
        "api_key=",
        "apikey=",
    ] {
        let mut search_from = 0;
        loop {
            let lowercase = redacted[search_from..].to_ascii_lowercase();
            let Some(offset) = lowercase.find(marker) else {
                break;
            };
            let start = search_from + offset;
            let value_start = start + marker.len();
            let value_end = redacted[value_start..]
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, '&' | ',' | ';' | '"' | '\'')
                })
                .map_or(redacted.len(), |offset| value_start + offset);
            redacted.replace_range(value_start..value_end, "[REDACTED]");
            search_from = value_start + "[REDACTED]".len();
            if search_from >= redacted.len() {
                break;
            }
        }
    }
    redacted
}

fn redact_url_userinfo(text: &str) -> String {
    let mut redacted = text.to_owned();
    let mut search_from = 0;
    while let Some(scheme_offset) = redacted[search_from..].find("://") {
        let authority_start = search_from + scheme_offset + 3;
        let authority_end = redacted[authority_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '/' | '?' | '#')
            })
            .map_or(redacted.len(), |offset| authority_start + offset);
        let Some(at_offset) = redacted[authority_start..authority_end].rfind('@') else {
            search_from = authority_end;
            continue;
        };
        let userinfo_end = authority_start + at_offset;
        redacted.replace_range(authority_start..userinfo_end, "[REDACTED]");
        search_from = authority_start + "[REDACTED]@".len();
    }
    redacted
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        truncated.push_str("...");
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(hub: &LogHub, message: &str) -> LogEntry {
        hub.record(
            LogLevel::Info,
            "keeppeek::test",
            message.to_owned(),
            BTreeMap::new(),
            None,
            None,
        )
    }

    #[test]
    fn evicts_oldest_entries_at_count_limit() {
        let hub = LogHub::new(LogHubLimits {
            max_entries: 2,
            ..LogHubLimits::default()
        });
        record(&hub, "one");
        record(&hub, "two");
        record(&hub, "three");

        let snapshot = hub.snapshot(None, 10);

        assert_eq!(
            snapshot
                .entries
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            ["two", "three"]
        );
        assert_eq!(snapshot.oldest_sequence, Some(2));
        assert!(snapshot.truncated);
    }

    #[test]
    fn evicts_entries_at_serialized_byte_limit() {
        let hub = LogHub::new(LogHubLimits {
            max_bytes: 300,
            ..LogHubLimits::default()
        });
        record(&hub, &"a".repeat(120));
        record(&hub, &"b".repeat(120));

        let snapshot = hub.snapshot(None, 10);

        assert!(snapshot.stats.byte_count <= 300);
        assert_eq!(snapshot.entries.last().unwrap().sequence, 2);
        assert!(snapshot.stats.evicted_entries > 0);
    }

    #[test]
    fn broadcasts_each_entry_to_each_subscriber() {
        let hub = LogHub::default();
        let first = hub.subscribe(None, 10).unwrap();
        let second = hub.subscribe(None, 10).unwrap();

        let entry = record(&hub, "broadcast");

        assert_eq!(
            first.next_timeout(Duration::from_millis(10)).unwrap(),
            entry
        );
        assert_eq!(
            second.next_timeout(Duration::from_millis(10)).unwrap(),
            entry
        );
    }

    #[test]
    fn slow_subscriber_drops_without_blocking_publishers() {
        let hub = LogHub::new(LogHubLimits {
            subscriber_capacity: 1,
            ..LogHubLimits::default()
        });
        let subscription = hub.subscribe(None, 10).unwrap();

        record(&hub, "queued");
        record(&hub, "dropped");

        assert_eq!(
            subscription
                .next_timeout(Duration::from_millis(10))
                .unwrap()
                .message,
            "queued"
        );
        assert_eq!(subscription.take_dropped(), 1);
    }

    #[test]
    fn redacts_sensitive_fields_and_url_userinfo() {
        let hub = LogHub::default();
        let mut fields = BTreeMap::new();
        fields.insert("password".to_owned(), Value::String("hunter2".to_owned()));
        fields.insert(
            "stream_url".to_owned(),
            Value::String("rtsp://operator:camera@192.0.2.1/live".to_owned()),
        );

        let entry = hub.record(
            LogLevel::Warn,
            "keeppeek::test",
            "request token=abc123".to_owned(),
            fields,
            None,
            None,
        );

        assert_eq!(entry.fields["password"], "[REDACTED]");
        assert_eq!(
            entry.fields["stream_url"],
            "rtsp://[REDACTED]@192.0.2.1/live"
        );
        assert_eq!(entry.message, "request token=[REDACTED]");
    }

    #[test]
    fn reports_replay_truncation_after_eviction() {
        let hub = LogHub::new(LogHubLimits {
            max_entries: 2,
            ..LogHubLimits::default()
        });
        record(&hub, "one");
        record(&hub, "two");
        record(&hub, "three");

        let mut subscription = hub.subscribe(Some(0), 10).unwrap();

        assert!(subscription.take_replay_truncated());
        assert_eq!(subscription.pop_replay().unwrap().sequence, 2);
        assert_eq!(subscription.pop_replay().unwrap().sequence, 3);
    }

    #[test]
    fn enforces_stream_limit_and_releases_dropped_subscribers() {
        let hub = LogHub::new(LogHubLimits {
            max_subscribers: 1,
            ..LogHubLimits::default()
        });
        let first = hub.subscribe(None, 10).unwrap();

        assert!(matches!(
            hub.subscribe(None, 10),
            Err(LogStreamError::LimitReached)
        ));
        drop(first);
        assert_eq!(hub.stats().active_streams, 0);
        assert!(hub.subscribe(None, 10).is_ok());
    }
}
