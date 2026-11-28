//! Capture-correlated `traces.jsonl` streaming with RAII `trace_filter` boost.
//!
//! Subscribes directly to [`LoggingService`] during a bounded camera capture,
//! temporarily boosts protocol tracing targets under a reentrant RAII guard,
//! filters log records for the active capture window and camera, and writes
//! bounded JSONL lines into `<capture-dir>/traces.jsonl`.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    thread::{self, JoinHandle},
};

use crossbeam_channel::{Receiver, Sender, select};
use serde_json::Value;

use crate::logging::{LogEntry, LogSubscription, LoggingService, TraceFilterGuard};

/// Output filename written inside `<capture-dir>` when the `traces` artifact is enabled.
pub const TRACE_FILENAME: &str = "traces.jsonl";

/// Default tracing directive applied during a `traces`-enabled capture when `trace_filter` is omitted.
pub const DEFAULT_CAPTURE_TRACE_FILTER: &str = concat!(
    "retina=trace,reo_proto=trace,onvif=trace,",
    "keeppeek::rtsp=trace,keeppeek::reolink=trace,keeppeek::isapi=trace,",
    "keeppeek::cameras=trace,keeppeek::camera_events=trace,",
    "keeppeek::camera_debug=debug,keeppeek::media_time=trace,keeppeek::keeppeek=trace"
);

const TRACE_BUDGET_MIN_BYTES: u64 = 2_048;
const TRACE_BUDGET_MAX_BYTES: u64 = 1_048_576;
const TRACE_FLUSH_INTERVAL_LINES: usize = 64;
const PROTOCOL_TARGET_PREFIXES: &[&str] = &[
    "retina",
    "reo_proto",
    "onvif",
    "keeppeek::rtsp",
    "keeppeek::reolink",
    "keeppeek::isapi",
    "keeppeek::cameras",
    "keeppeek::camera_events",
    "keeppeek::camera_debug",
    "keeppeek::media_time",
    "keeppeek::storage",
    "keeppeek::keeppeek",
];

const _: () = {
    assert!(TRACE_BUDGET_MIN_BYTES <= TRACE_BUDGET_MAX_BYTES);
    assert!(TRACE_FLUSH_INTERVAL_LINES > 0);
};

/// Active per-capture trace recorder that drains `LogHub` entries and restores `trace_filter` on drop.
pub(crate) struct TraceSession {
    stop_tx: Option<Sender<()>>,
    worker: Option<JoinHandle<TraceDrainSummary>>,
    _filter_guard: Option<TraceFilterGuard>,
    startup_warnings: Vec<String>,
}

struct TraceDrainSummary {
    truncated: bool,
    dropped: u64,
    write_error: Option<String>,
}

impl TraceSession {
    /// Starts trace boosting and live JSONL streaming when `logging` is available.
    #[must_use]
    pub(crate) fn start(
        logging: Option<&LoggingService>,
        capture_dir: &Path,
        camera_id: &str,
        capture_id: &str,
        trace_filter: Option<&str>,
        budget_bytes: u64,
    ) -> Self {
        let Some(logging) = logging else {
            return Self::inactive(Vec::new());
        };
        let mut startup_warnings = Vec::new();
        let directive = trace_filter
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_CAPTURE_TRACE_FILTER);
        let filter_guard = match logging.acquire_trace_filter_boost(directive) {
            Ok(guard) => Some(guard),
            Err(error) => {
                startup_warnings.push(format!(
                    "unable to apply capture trace_filter boost: {error}"
                ));
                None
            }
        };
        let subscription = match logging.subscribe_live() {
            Ok(sub) => sub,
            Err(error) => {
                startup_warnings.push(format!("unable to subscribe to live trace stream: {error}"));
                return Self::inactive(startup_warnings);
            }
        };
        let trace_path = capture_dir.join(TRACE_FILENAME);
        let file = match File::create(&trace_path) {
            Ok(file) => file,
            Err(error) => {
                startup_warnings.push(format!("unable to create {TRACE_FILENAME}: {error}"));
                return Self::inactive(startup_warnings);
            }
        };
        let (stop_tx, stop_rx) = crossbeam_channel::bounded(1);
        let worker = spawn_drainer(
            subscription,
            stop_rx,
            BufWriter::new(file),
            camera_id.to_owned(),
            capture_id.to_owned(),
            budget_bytes.clamp(TRACE_BUDGET_MIN_BYTES, TRACE_BUDGET_MAX_BYTES),
        );
        match worker {
            Ok(handle) => Self {
                stop_tx: Some(stop_tx),
                worker: Some(handle),
                _filter_guard: filter_guard,
                startup_warnings,
            },
            Err(error) => {
                startup_warnings.push(format!("unable to spawn trace capture worker: {error}"));
                Self::inactive(startup_warnings)
            }
        }
    }

    const fn inactive(startup_warnings: Vec<String>) -> Self {
        Self {
            stop_tx: None,
            worker: None,
            _filter_guard: None,
            startup_warnings,
        }
    }

    /// Stops streaming, flushes `traces.jsonl`, restores `trace_filter`, and returns warnings.
    #[must_use]
    pub(crate) fn finish(mut self) -> Vec<String> {
        let mut warnings = std::mem::take(&mut self.startup_warnings);
        if let Some(summary) = self.stop_and_join() {
            if summary.truncated {
                warnings.push(
                    "traces.jsonl reached byte budget and truncated remaining entries".to_owned(),
                );
            }
            if summary.dropped > 0 {
                warnings.push(format!(
                    "traces.jsonl dropped {} log entries due to subscriber queue capacity",
                    summary.dropped
                ));
            }
            if let Some(error) = summary.write_error {
                warnings.push(format!("traces.jsonl write error: {error}"));
            }
        }
        // Explicitly release the filter boost after the drainer stops and flushes.
        drop(self._filter_guard.take());
        warnings
    }

    fn stop_and_join(&mut self) -> Option<TraceDrainSummary> {
        drop(self.stop_tx.take());
        self.worker.take().and_then(|handle| handle.join().ok())
    }
}

impl Drop for TraceSession {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

fn spawn_drainer(
    subscription: LogSubscription,
    stop_rx: Receiver<()>,
    writer: BufWriter<File>,
    camera_id: String,
    capture_id: String,
    budget_bytes: u64,
) -> std::io::Result<JoinHandle<TraceDrainSummary>> {
    thread::Builder::new()
        .name("keeppeek-trace-capture".to_owned())
        .spawn(move || {
            run_trace_drainer(
                &subscription,
                &stop_rx,
                writer,
                &camera_id,
                &capture_id,
                budget_bytes,
            )
        })
}

fn run_trace_drainer(
    subscription: &LogSubscription,
    stop_rx: &Receiver<()>,
    mut writer: BufWriter<File>,
    camera_id: &str,
    capture_id: &str,
    budget_bytes: u64,
) -> TraceDrainSummary {
    let mut state = DrainWriteState {
        written_bytes: 0,
        budget_bytes,
        truncated: false,
        lines_since_flush: 0,
        write_error: None,
    };
    loop {
        select! {
            recv(stop_rx) -> _ => break,
            recv(subscription.receiver()) -> msg => match msg {
                Ok(entry) => {
                    record_matching_entry(&entry, camera_id, capture_id, &mut writer, &mut state);
                }
                Err(_) => break,
            }
        }
    }
    while let Ok(entry) = subscription.try_next() {
        record_matching_entry(&entry, camera_id, capture_id, &mut writer, &mut state);
    }
    if state.write_error.is_none()
        && let Err(error) = writer.flush()
    {
        state.write_error = Some(error.to_string());
    }
    TraceDrainSummary {
        truncated: state.truncated,
        dropped: subscription.take_dropped(),
        write_error: state.write_error,
    }
}

struct DrainWriteState {
    written_bytes: u64,
    budget_bytes: u64,
    truncated: bool,
    lines_since_flush: usize,
    write_error: Option<String>,
}

fn record_matching_entry(
    entry: &LogEntry,
    camera_id: &str,
    capture_id: &str,
    writer: &mut BufWriter<File>,
    state: &mut DrainWriteState,
) {
    if state.truncated || state.write_error.is_some() {
        return;
    }
    if !entry_matches_capture(entry, camera_id, capture_id) {
        return;
    }
    let Ok(mut line) = serde_json::to_vec(entry) else {
        return;
    };
    line.push(b'\n');
    let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);
    if state.written_bytes.saturating_add(line_len) > state.budget_bytes {
        state.truncated = true;
        return;
    }
    if let Err(error) = writer.write_all(&line) {
        state.write_error = Some(error.to_string());
        return;
    }
    state.written_bytes = state.written_bytes.saturating_add(line_len);
    state.lines_since_flush += 1;
    if state.lines_since_flush >= TRACE_FLUSH_INTERVAL_LINES {
        state.lines_since_flush = 0;
        if let Err(error) = writer.flush() {
            state.write_error = Some(error.to_string());
        }
    }
}

/// Returns `true` when `entry` belongs to the active capture, matches `camera_id`, or is an untagged protocol log.
#[must_use]
pub(crate) fn entry_matches_capture(entry: &LogEntry, camera_id: &str, capture_id: &str) -> bool {
    if let Some(entry_capture_id) = entry.fields.get("capture_id").and_then(Value::as_str) {
        return entry_capture_id == capture_id;
    }
    if let Some(entry_camera_id) = entry.fields.get("camera_id").and_then(Value::as_str)
        && entry_camera_id != "[REDACTED]"
    {
        return entry_camera_id == camera_id;
    }
    is_protocol_or_camera_target(&entry.target)
}

fn is_protocol_or_camera_target(target: &str) -> bool {
    PROTOCOL_TARGET_PREFIXES.iter().any(|prefix| {
        target == *prefix
            || target
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with("::"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::{LogFilterFile, LogLevel};
    use std::{collections::BTreeMap, path::PathBuf};

    fn make_entry(target: &str, fields: &[(&str, &str)], message: &str) -> LogEntry {
        let mut map = BTreeMap::new();
        for (key, value) in fields {
            map.insert((*key).to_owned(), Value::String((*value).to_owned()));
        }
        LogEntry {
            sequence: 1,
            timestamp_ms: 1_000,
            level: LogLevel::Trace,
            target: target.to_owned(),
            message: message.to_owned(),
            fields: map,
            file: None,
            line: None,
        }
    }

    #[test]
    fn entry_filter_isolates_concurrent_captures_and_keeps_protocol_logs() {
        let own_cap = make_entry(
            "keeppeek::rtsp",
            &[("capture_id", "cap-1"), ("camera_id", "cam-1")],
            "a",
        );
        let other_cap = make_entry(
            "keeppeek::rtsp",
            &[("capture_id", "cap-2"), ("camera_id", "cam-2")],
            "b",
        );
        let own_cam = make_entry("keeppeek::cameras", &[("camera_id", "cam-1")], "c");
        let other_cam = make_entry("keeppeek::cameras", &[("camera_id", "cam-2")], "d");
        let redacted_cam_proto = make_entry("retina::codec", &[("camera_id", "[REDACTED]")], "e");
        let untagged_proto = make_entry("reo_proto::session", &[], "f");
        let unrelated = make_entry("keeppeek::server", &[], "g");

        assert!(entry_matches_capture(&own_cap, "cam-1", "cap-1"));
        assert!(!entry_matches_capture(&other_cap, "cam-1", "cap-1"));
        assert!(entry_matches_capture(&own_cam, "cam-1", "cap-1"));
        assert!(!entry_matches_capture(&other_cam, "cam-1", "cap-1"));
        assert!(entry_matches_capture(
            &redacted_cam_proto,
            "192.0.2.10",
            "cap-1"
        ));
        assert!(entry_matches_capture(&untagged_proto, "cam-1", "cap-1"));
        assert!(!entry_matches_capture(&unrelated, "cam-1", "cap-1"));
    }

    #[test]
    fn trace_session_streams_bounded_jsonl_and_restores_filter() {
        let dir = std::env::temp_dir().join(format!("keeppeek-traces-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (logging, dispatch) = LoggingService::for_test(
            LogFilterFile::new(dir.join("log-filter")),
            "info,keeppeek=debug",
        );

        let session = TraceSession::start(
            Some(&logging),
            &dir,
            "cam-1",
            "cap-1",
            Some("retina=trace,keeppeek::rtsp=trace"),
            TRACE_BUDGET_MIN_BYTES,
        );
        assert!(logging.active_filter().contains("retina=trace"));

        tracing::dispatcher::with_default(&dispatch, || {
            let span = tracing::info_span!("cap", camera_id = "cam-1", capture_id = "cap-1");
            let _guard = span.enter();
            tracing::trace!(target: "keeppeek::rtsp", "rtsp trace event");
            let other_span =
                tracing::info_span!("other", camera_id = "cam-2", capture_id = "cap-2");
            let _other = other_span.enter();
            tracing::trace!(target: "keeppeek::rtsp", "other camera event");
        });

        let warnings = session.finish();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(logging.active_filter(), "info,keeppeek=debug");

        let contents = std::fs::read_to_string(PathBuf::from(&dir).join(TRACE_FILENAME)).unwrap();
        assert!(contents.contains("rtsp trace event"));
        assert!(!contents.contains("other camera event"));
        let line_count = contents.lines().count();
        assert_eq!(line_count, 1);
        let parsed: LogEntry = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(parsed.fields["camera_id"], "cam-1");
        assert_eq!(parsed.fields["capture_id"], "cap-1");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
