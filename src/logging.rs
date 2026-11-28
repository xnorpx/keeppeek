use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(any(windows, test))]
use std::fs::File;

use serde::Serialize;
use tracing::info;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::{
    EnvFilter, Registry,
    fmt::MakeWriter,
    prelude::*,
    reload::{self, Handle},
    util::SubscriberInitExt,
};

use crate::config;
#[cfg(windows)]
use crate::config::ServiceLogDestination;

mod console;
mod hub;
mod layer;
mod stream;

pub(crate) use console::ConsolePause;
pub(crate) use hub::LogSubscription;
pub use hub::{
    LogBufferStats, LogEntry, LogHub, LogHubLimits, LogLevel, LogSnapshot, LogStreamError,
};
pub use layer::LogCaptureLayer;
pub use stream::LogStreamReader;

pub const DEFAULT_LOG_FILTER: &str = "info,keeppeek=debug";
pub const TRACE_FILTER_MAX_BYTES: usize = 512;
const MAX_ACTIVE_TRACE_BOOSTS: usize = 32;

const _: () = {
    assert!(TRACE_FILTER_MAX_BYTES > 0);
    assert!(MAX_ACTIVE_TRACE_BOOSTS > 0);
};

#[derive(Clone)]
pub struct LoggingService {
    hub: LogHub,
    filter_file: LogFilterFile,
    reload_handle: Handle<EnvFilter, Registry>,
    active_filter: Arc<Mutex<String>>,
    filter_error: Arc<Mutex<Option<String>>>,
    update_lock: Arc<Mutex<()>>,
    boost_state: Arc<Mutex<FilterBoostState>>,
}

#[derive(Default)]
struct FilterBoostState {
    base_filter: String,
    next_token: u64,
    active_boosts: Vec<(u64, String)>,
}

/// RAII guard that removes a temporary trace-filter boost and restores the prior filter on drop.
pub struct TraceFilterGuard {
    service: LoggingService,
    token: u64,
}

impl std::fmt::Debug for LoggingService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoggingService")
            .field("active_filter", &self.active_filter())
            .field("filter_file", &self.filter_file)
            .finish()
    }
}

impl Drop for TraceFilterGuard {
    fn drop(&mut self) {
        self.service.release_trace_filter_boost(self.token);
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LoggingSettings {
    pub active_filter: String,
    pub default_filter: &'static str,
    pub filter_error: Option<String>,
    pub version: &'static str,
    pub buffer: LogBufferStats,
}

pub fn initialize_global_logging(config_path: &Path) -> anyhow::Result<LoggingService> {
    let filter_file = LogFilterFile::beside_config(config_path);
    let initial_filter = resolve_initial_filter(&filter_file, std::env::var("RUST_LOG").ok());
    let env_filter = EnvFilter::try_new(&initial_filter.directive)?;
    let (filter_layer, reload_handle) = reload::Layer::new(env_filter);
    let hub = LogHub::default();
    configure_secret_redaction(&hub, config_path)?;

    let writer = RedactingMakeWriter::stderr(hub.clone());
    Registry::default()
        .with(filter_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer.clone())
                .with_filter(filter_fn(|metadata| !source_timing_event(metadata))),
        )
        .with(LogCaptureLayer::with_formatted_timing(writer))
        .try_init()?;

    Ok(logging_service(
        hub,
        filter_file,
        reload_handle,
        initial_filter.directive,
        initial_filter.error,
    ))
}

#[cfg(windows)]
pub fn initialize_service_logging(
    config_path: &Path,
    destination: ServiceLogDestination,
) -> anyhow::Result<LoggingService> {
    let filter_file = LogFilterFile::beside_config(config_path);
    let initial_filter = resolve_initial_filter(&filter_file, std::env::var("RUST_LOG").ok());
    let env_filter = EnvFilter::try_new(&initial_filter.directive)?;
    let (filter_layer, reload_handle) = reload::Layer::new(env_filter);
    let hub = LogHub::default();
    configure_secret_redaction(&hub, config_path)?;

    match destination {
        ServiceLogDestination::EventLog => {
            let event_log = tracing_layer_win_eventlog::EventLogLayer::new("KeepPeekService")?;
            Registry::default()
                .with(filter_layer)
                .with(event_log)
                .with(LogCaptureLayer::new(hub.clone()))
                .try_init()?;
        }
        ServiceLogDestination::File => {
            let log_directory = config::ensure_config_dir()?;
            let log_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_directory.join("keeppeek-service.log"))?;
            let writer = RedactingMakeWriter::file(hub.clone(), log_file);
            Registry::default()
                .with(filter_layer)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(writer.clone())
                        .with_filter(filter_fn(|metadata| !source_timing_event(metadata))),
                )
                .with(LogCaptureLayer::with_formatted_timing(writer))
                .try_init()?;
        }
    }

    Ok(logging_service(
        hub,
        filter_file,
        reload_handle,
        initial_filter.directive,
        initial_filter.error,
    ))
}

fn configure_secret_redaction(hub: &LogHub, config_path: &Path) -> anyhow::Result<()> {
    let mut values = config::load_secrets(config_path)?.redaction_values();
    if let Ok(cameras) = config::load_cameras(config_path) {
        for camera in cameras.into_values().flatten() {
            values.push(camera.ip.to_string());
            values.push(camera.username);
            values.push(camera.password);
            values.extend(camera.uid);
            values.extend(camera.main_rtsp_url);
            values.extend(camera.sub_rtsp_url);
        }
    }
    values.push(config_path.to_string_lossy().into_owned());
    if let Some(parent) = config_path
        .parent()
        .filter(|parent| *parent != Path::new("/"))
    {
        values.push(parent.to_string_lossy().into_owned());
    }
    if let Ok(configuration) = config::load_config(config_path) {
        values.extend(configuration.storage.medium_term_path);
        values.extend(configuration.storage.long_term_path);
        values.extend(configuration.storage.recording_catalog_path);
        values.extend(configuration.storage.event_thumbnail_path);
    }
    hub.set_sensitive_values(values);
    Ok(())
}

#[derive(Clone)]
struct RedactingMakeWriter {
    hub: LogHub,
    destination: RedactingDestination,
}

#[derive(Clone)]
enum RedactingDestination {
    Stderr,
    #[cfg(any(windows, test))]
    File(Arc<Mutex<File>>),
}

impl RedactingMakeWriter {
    const fn stderr(hub: LogHub) -> Self {
        Self {
            hub,
            destination: RedactingDestination::Stderr,
        }
    }

    #[cfg(any(windows, test))]
    fn file(hub: LogHub, file: File) -> Self {
        Self {
            hub,
            destination: RedactingDestination::File(Arc::new(Mutex::new(file))),
        }
    }
}

impl<'writer> MakeWriter<'writer> for RedactingMakeWriter {
    type Writer = RedactingWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        RedactingWriter {
            hub: self.hub.clone(),
            destination: self.destination.clone(),
            buffer: Vec::new(),
        }
    }
}

struct RedactingWriter {
    hub: LogHub,
    destination: RedactingDestination,
    buffer: Vec<u8>,
}

impl Write for RedactingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn source_timing_event(metadata: &tracing::Metadata<'_>) -> bool {
    metadata.name() == "camera.rtsp.source_timing"
}

impl Drop for RedactingWriter {
    fn drop(&mut self) {
        let output = self
            .hub
            .redact_output(String::from_utf8_lossy(&self.buffer).as_ref());
        self.destination.write(output.as_bytes());
    }
}

impl RedactingDestination {
    fn write(&self, output: &[u8]) {
        match self {
            Self::Stderr => console::write(output),
            #[cfg(any(windows, test))]
            Self::File(file) => {
                let _ = file
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .write_all(output);
            }
        }
    }
}

fn logging_service(
    hub: LogHub,
    filter_file: LogFilterFile,
    reload_handle: Handle<EnvFilter, Registry>,
    directive: String,
    error: Option<String>,
) -> LoggingService {
    let service = LoggingService::new(hub, filter_file, reload_handle, directive, error);
    if let Some(error) = service.filter_error() {
        tracing::warn!(%error, "using fallback log filter");
    }
    service
}

impl LoggingService {
    fn new(
        hub: LogHub,
        filter_file: LogFilterFile,
        reload_handle: Handle<EnvFilter, Registry>,
        active_filter: String,
        filter_error: Option<String>,
    ) -> Self {
        Self {
            hub,
            filter_file,
            reload_handle,
            active_filter: Arc::new(Mutex::new(active_filter.clone())),
            filter_error: Arc::new(Mutex::new(filter_error)),
            update_lock: Arc::new(Mutex::new(())),
            boost_state: Arc::new(Mutex::new(FilterBoostState {
                base_filter: active_filter,
                next_token: 1,
                active_boosts: Vec::new(),
            })),
        }
    }

    pub(crate) fn dashboard_redactor(&self) -> LogHub {
        self.hub.clone()
    }

    pub(crate) fn redact_secret(&self, value: &str) {
        self.hub.add_sensitive_value(value.to_owned());
    }

    pub fn settings(&self) -> LoggingSettings {
        LoggingSettings {
            active_filter: self.active_filter(),
            default_filter: DEFAULT_LOG_FILTER,
            filter_error: self.filter_error(),
            version: env!("CARGO_PKG_VERSION"),
            buffer: self.hub.stats(),
        }
    }

    pub fn active_filter(&self) -> String {
        self.active_filter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn base_filter(&self) -> String {
        self.boost_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .base_filter
            .clone()
    }

    pub fn filter_error(&self) -> Option<String> {
        self.filter_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn update_filter(&self, log_filter: &str) -> anyhow::Result<()> {
        let log_filter = log_filter.trim();
        if log_filter.is_empty() {
            anyhow::bail!("log filter must not be empty");
        }
        EnvFilter::try_new(log_filter)?;

        let _update = self
            .update_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let effective = {
            let boost = self
                .boost_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            compose_boosted_filter(log_filter, &boost.active_boosts)
        };
        let env_filter = EnvFilter::try_new(&effective)?;
        let previous_file = std::fs::read(self.filter_file.path()).ok();
        self.filter_file.write_log_filter(log_filter)?;
        if let Err(error) = self.reload_handle.reload(env_filter) {
            match previous_file {
                Some(previous_file) => {
                    config::write_private_file_atomically(self.filter_file.path(), &previous_file)?;
                }
                None => match std::fs::remove_file(self.filter_file.path()) {
                    Ok(()) => {}
                    Err(remove_error) if remove_error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(remove_error) => return Err(remove_error.into()),
                },
            }
            return Err(error.into());
        }
        info!(%log_filter, "reloaded log filter");

        self.boost_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .base_filter = log_filter.to_owned();
        *self
            .active_filter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = effective;
        *self
            .filter_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        Ok(())
    }

    /// Temporarily applies `directive` on top of the active filter until the returned guard drops.
    ///
    /// # Errors
    /// Returns an error if `directive` is empty, oversized, invalid as an `EnvFilter`,
    /// or the maximum number of concurrent boosts is already active.
    pub fn acquire_trace_filter_boost(&self, directive: &str) -> anyhow::Result<TraceFilterGuard> {
        let directive = directive.trim();
        anyhow::ensure!(!directive.is_empty(), "trace_filter must not be empty");
        anyhow::ensure!(
            directive.len() <= TRACE_FILTER_MAX_BYTES,
            "trace_filter must be at most {TRACE_FILTER_MAX_BYTES} bytes"
        );
        EnvFilter::try_new(directive)?;

        let _update = self
            .update_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut boost = self
            .boost_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(
            boost.active_boosts.len() < MAX_ACTIVE_TRACE_BOOSTS,
            "too many concurrent trace_filter boosts"
        );
        let token = boost.next_token;
        boost.next_token = boost.next_token.saturating_add(1);
        boost.active_boosts.push((token, directive.to_owned()));
        let effective = compose_boosted_filter(&boost.base_filter, &boost.active_boosts);
        let env_filter = match EnvFilter::try_new(&effective) {
            Ok(filter) => filter,
            Err(error) => {
                boost.active_boosts.retain(|(id, _)| *id != token);
                return Err(error.into());
            }
        };
        if let Err(error) = self.reload_handle.reload(env_filter) {
            boost.active_boosts.retain(|(id, _)| *id != token);
            return Err(error.into());
        }
        *self
            .active_filter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = effective;
        Ok(TraceFilterGuard {
            service: self.clone(),
            token,
        })
    }

    fn release_trace_filter_boost(&self, token: u64) {
        let _update = self
            .update_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut boost = self
            .boost_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        boost.active_boosts.retain(|(id, _)| *id != token);
        let effective = compose_boosted_filter(&boost.base_filter, &boost.active_boosts);
        if let Ok(env_filter) = EnvFilter::try_new(&effective)
            && self.reload_handle.reload(env_filter).is_ok()
        {
            *self
                .active_filter
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = effective;
        }
    }

    pub fn snapshot(&self, after: Option<u64>, limit: usize) -> LogSnapshot {
        self.hub.snapshot(after, limit)
    }

    pub(crate) fn subscribe_live(&self) -> Result<LogSubscription, LogStreamError> {
        self.hub.subscribe_live()
    }

    pub fn stream(
        &self,
        after: Option<u64>,
        tail: usize,
    ) -> Result<LogStreamReader, LogStreamError> {
        let subscription = self.hub.subscribe(after, tail)?;
        Ok(LogStreamReader::new(subscription, Duration::from_secs(15)))
    }

    pub fn close_streams(&self) {
        self.hub.close();
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        filter_file: LogFilterFile,
        initial_filter: &str,
    ) -> (Self, tracing::Dispatch) {
        let hub = LogHub::default();
        let (filter_layer, reload_handle) =
            reload::Layer::new(EnvFilter::try_new(initial_filter).unwrap());
        let subscriber = Registry::default()
            .with(filter_layer)
            .with(LogCaptureLayer::new(hub.clone()));
        (
            Self::new(
                hub,
                filter_file,
                reload_handle,
                initial_filter.to_owned(),
                None,
            ),
            tracing::Dispatch::new(subscriber),
        )
    }
}

fn compose_boosted_filter(base_filter: &str, boosts: &[(u64, String)]) -> String {
    if boosts.is_empty() {
        return base_filter.to_owned();
    }
    let mut combined = String::from(base_filter);
    for (_, directive) in boosts {
        if !directive.is_empty() {
            combined.push(',');
            combined.push_str(directive);
        }
    }
    combined
}

struct InitialFilter {
    directive: String,
    error: Option<String>,
}

fn resolve_initial_filter(
    filter_file: &LogFilterFile,
    environment_filter: Option<String>,
) -> InitialFilter {
    match filter_file.read_log_filter() {
        Ok(log_filter) => match EnvFilter::try_new(&log_filter) {
            Ok(_) => {
                return InitialFilter {
                    directive: log_filter,
                    error: None,
                };
            }
            Err(error) => {
                return fallback_initial_filter(
                    environment_filter,
                    Some(format!(
                        "invalid saved log filter in {}: {error}",
                        filter_file.path().display()
                    )),
                );
            }
        },
        Err(error) => {
            let missing = error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound);
            if !missing {
                return fallback_initial_filter(
                    environment_filter,
                    Some(format!(
                        "unable to read saved log filter from {}: {error}",
                        filter_file.path().display()
                    )),
                );
            }
        }
    }
    fallback_initial_filter(environment_filter, None)
}

fn fallback_initial_filter(
    environment_filter: Option<String>,
    mut error: Option<String>,
) -> InitialFilter {
    if let Some(environment_filter) = environment_filter {
        match EnvFilter::try_new(&environment_filter) {
            Ok(_) => {
                return InitialFilter {
                    directive: environment_filter,
                    error,
                };
            }
            Err(environment_error) => {
                let message = format!("invalid RUST_LOG value: {environment_error}");
                error = Some(
                    error.map_or_else(|| message.clone(), |error| format!("{error}; {message}")),
                );
            }
        }
    }
    InitialFilter {
        directive: DEFAULT_LOG_FILTER.to_owned(),
        error,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFilterFile {
    path: PathBuf,
}

impl LogFilterFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn beside_config(config_path: &Path) -> Self {
        let directory = config_path.parent().unwrap_or_else(|| Path::new("."));
        Self::new(directory.join("log-filter"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read_log_filter(&self) -> anyhow::Result<String> {
        let log_filter = std::fs::read_to_string(&self.path)?;
        let log_filter = log_filter.trim();
        if log_filter.is_empty() {
            anyhow::bail!("log filter file is empty");
        }
        Ok(log_filter.to_owned())
    }

    pub fn write_log_filter(&self, log_filter: &str) -> anyhow::Result<()> {
        let log_filter = log_filter.trim();
        if log_filter.is_empty() {
            anyhow::bail!("log filter must not be empty");
        }
        EnvFilter::try_new(log_filter)?;
        config::write_private_file_atomically(&self.path, log_filter.as_bytes())?;
        Ok(())
    }
}

pub fn read_log_filter_file_and_reload(
    log_filter_file: &LogFilterFile,
    trace_reload_handle: &Handle<EnvFilter, Registry>,
) -> anyhow::Result<()> {
    let log_filter = log_filter_file.read_log_filter()?;
    let env_filter = EnvFilter::try_new(&log_filter)?;
    trace_reload_handle.reload(env_filter)?;
    info!(%log_filter, "reloaded log filter");
    Ok(())
}

#[cfg(test)]
pub(crate) fn capture_timing_for_test(hub: LogHub, file: File, action: impl FnOnce()) {
    let writer = RedactingMakeWriter::file(hub, file);
    let subscriber = tracing_subscriber::Registry::default()
        .with(EnvFilter::new("warn,keeppeek::rtsp::timing=info"))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer.clone())
                .with_filter(filter_fn(|metadata| !source_timing_event(metadata))),
        )
        .with(LogCaptureLayer::with_formatted_timing(writer));
    tracing::subscriber::with_default(subscriber, action);
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use tracing::{Event, Subscriber, dispatcher::Dispatch};
    use tracing_subscriber::{Layer, layer::Context};

    use super::*;

    static NEXT_TEMPORARY_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Default)]
    struct EventTargets(Arc<Mutex<Vec<String>>>);

    impl<S> Layer<S> for EventTargets
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            self.0
                .lock()
                .unwrap()
                .push(event.metadata().target().to_owned());
        }
    }

    fn temporary_filter_file() -> LogFilterFile {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = NEXT_TEMPORARY_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-log-filter-{}-{unique}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        LogFilterFile::new(directory.join("log-filter"))
    }

    fn test_dispatch(
        filter: &str,
        targets: EventTargets,
    ) -> (Dispatch, Handle<EnvFilter, Registry>) {
        let (filter_layer, handle) = reload::Layer::new(EnvFilter::try_new(filter).unwrap());
        let subscriber = Registry::default().with(filter_layer).with(targets);
        (Dispatch::new(subscriber), handle)
    }

    #[test]
    fn reloads_filter_from_file_synchronously() {
        let file = temporary_filter_file();
        file.write_log_filter("info,str0m=warn").unwrap();
        let targets = EventTargets::default();
        let (dispatch, handle) = test_dispatch("error", targets.clone());

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(target: "keeppeek::test", "before reload");
            read_log_filter_file_and_reload(&file, &handle).unwrap();
            tracing::info!(target: "keeppeek::test", "after reload");
            tracing::info!(target: "str0m", "filtered after reload");
            tracing::warn!(target: "str0m", "included after reload");
        });

        assert_eq!(
            *targets.0.lock().unwrap(),
            ["keeppeek::logging", "keeppeek::test", "str0m"]
        );
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn invalid_filter_preserves_active_filter() {
        let file = temporary_filter_file();
        fs::write(file.path(), "info,keeppeek=verbose").unwrap();
        let targets = EventTargets::default();
        let (dispatch, handle) = test_dispatch("warn", targets.clone());

        tracing::dispatcher::with_default(&dispatch, || {
            assert!(read_log_filter_file_and_reload(&file, &handle).is_err());
            tracing::info!(target: "keeppeek::test", "still filtered");
            tracing::warn!(target: "keeppeek::test", "still included");
        });

        assert_eq!(*targets.0.lock().unwrap(), ["keeppeek::test"]);
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn missing_filter_file_does_not_reload() {
        let file = temporary_filter_file();
        let targets = EventTargets::default();
        let (dispatch, handle) = test_dispatch("error", targets.clone());

        tracing::dispatcher::with_default(&dispatch, || {
            assert!(read_log_filter_file_and_reload(&file, &handle).is_err());
            tracing::warn!(target: "keeppeek::test", "still filtered");
            tracing::error!(target: "keeppeek::test", "still included");
        });

        assert_eq!(*targets.0.lock().unwrap(), ["keeppeek::test"]);
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn saved_filter_takes_precedence_over_environment() {
        let file = temporary_filter_file();
        file.write_log_filter("debug,retina=info").unwrap();

        let initial = resolve_initial_filter(&file, Some("warn".to_owned()));

        assert_eq!(initial.directive, "debug,retina=info");
        assert_eq!(initial.error, None);
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn environment_filter_is_used_when_saved_filter_is_missing() {
        let file = temporary_filter_file();

        let initial = resolve_initial_filter(&file, Some("info,str0m=warn".to_owned()));

        assert_eq!(initial.directive, "info,str0m=warn");
        assert_eq!(initial.error, None);
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn invalid_saved_and_environment_filters_fall_back_to_default() {
        let file = temporary_filter_file();
        fs::write(file.path(), "keeppeek=verbose").unwrap();

        let initial = resolve_initial_filter(&file, Some("str0m=verbose".to_owned()));

        assert_eq!(initial.directive, DEFAULT_LOG_FILTER);
        let error = initial.error.unwrap();
        assert!(error.contains("invalid saved log filter"));
        assert!(error.contains("invalid RUST_LOG value"));
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn packet_clock_fields_preserve_numeric_values_in_both_sinks_and_redact_strings() {
        let file = temporary_filter_file();
        let path = file.path().with_file_name("timing.log");
        let hub = LogHub::default();
        hub.set_sensitive_values(["1234".to_owned(), "192.0.2.1".to_owned()]);
        let writer = RedactingMakeWriter::file(hub.clone(), File::create(&path).unwrap());
        let subscriber = Registry::default()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(writer.clone())
                    .with_filter(filter_fn(|metadata| !source_timing_event(metadata))),
            )
            .with(LogCaptureLayer::with_formatted_timing(writer));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(name:"camera.rtsp.source_timing", first_rtp_ticks=12345_u32, camera_id="192.0.2.1",password=1234_u64,"RTSP source timing");
        });
        let formatted: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let captured = hub.snapshot(None, 1).entries.pop().unwrap();
        assert_eq!(
            formatted["fields"],
            serde_json::to_value(&captured.fields).unwrap()
        );
        assert_eq!(captured.fields["first_rtp_ticks"], 12345);
        assert_eq!(captured.fields["camera_id"], "[REDACTED]");
        assert_eq!(captured.fields["password"], "[REDACTED]");
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn formatted_output_redacts_configured_camera_values() {
        let file = temporary_filter_file();
        let output_path = file.path().with_file_name("formatted.log");
        let hub = LogHub::default();
        hub.set_sensitive_values([
            "192.168.2.44".to_owned(),
            "rtsp://admin:camera-secret@192.168.2.44/main".to_owned(),
            "camera-secret".to_owned(),
        ]);
        let writer = RedactingMakeWriter::file(hub, File::create(&output_path).unwrap());
        {
            let mut event = writer.make_writer();
            writeln!(
                event,
                "camera=192.168.2.44 endpoint=rtsp://admin:camera-secret@192.168.2.44/main"
            )
            .unwrap();
        }

        let output = fs::read_to_string(&output_path).unwrap();
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("192.168.2.44"));
        assert!(!output.contains("camera-secret"));
        assert!(!output.contains("rtsp://"));
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn console_pause_preserves_file_sink_and_log_hub_redaction() {
        let file = temporary_filter_file();
        let output_path = file.path().with_file_name("paused-service.log");
        let hub = LogHub::default();
        hub.set_sensitive_values(["secret-camera-token".to_owned()]);
        let pause = ConsolePause::acquire().unwrap();
        let writer = RedactingMakeWriter::file(hub.clone(), File::create(&output_path).unwrap());
        let subscriber = Registry::default()
            .with(tracing_subscriber::fmt::layer().with_writer(writer))
            .with(LogCaptureLayer::new(hub.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("camera credential secret-camera-token remained active");
        });
        drop(pause);

        let file_output = fs::read_to_string(&output_path).unwrap();
        assert!(file_output.contains("[REDACTED]"));
        assert!(!file_output.contains("secret-camera-token"));
        let entries = hub.snapshot(None, 10).entries;
        assert!(
            entries
                .iter()
                .any(|entry| entry.message.contains("[REDACTED]"))
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.message.contains("secret-camera-token"))
        );
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn trace_filter_boost_is_reentrant_and_restores_on_drop_and_panic() {
        let file = temporary_filter_file();
        file.write_log_filter("info,keeppeek=debug").unwrap();
        let (service, dispatch) = LoggingService::for_test(file.clone(), "info,keeppeek=debug");

        let boost_a = service.acquire_trace_filter_boost("retina=trace").unwrap();
        assert_eq!(service.active_filter(), "info,keeppeek=debug,retina=trace");

        let boost_b = service
            .acquire_trace_filter_boost("keeppeek::rtsp=trace")
            .unwrap();
        assert_eq!(
            service.active_filter(),
            "info,keeppeek=debug,retina=trace,keeppeek::rtsp=trace"
        );

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::trace!(target: "retina::test", "retina trace enabled");
            tracing::trace!(target: "keeppeek::rtsp", "rtsp trace enabled");
        });

        drop(boost_a);
        assert_eq!(
            service.active_filter(),
            "info,keeppeek=debug,keeppeek::rtsp=trace"
        );

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _boost_c = service.acquire_trace_filter_boost("onvif=trace").unwrap();
            assert_eq!(
                service.active_filter(),
                "info,keeppeek=debug,keeppeek::rtsp=trace,onvif=trace"
            );
            panic!("simulated capture panic");
        }));
        assert!(panicked.is_err());
        assert_eq!(
            service.active_filter(),
            "info,keeppeek=debug,keeppeek::rtsp=trace"
        );

        drop(boost_b);
        assert_eq!(service.active_filter(), "info,keeppeek=debug");
        assert_eq!(file.read_log_filter().unwrap(), "info,keeppeek=debug");

        let entries = service.snapshot(None, 10).entries;
        assert!(entries.iter().any(|e| e.target == "retina::test"));
        assert!(entries.iter().any(|e| e.target == "keeppeek::rtsp"));
        assert!(
            service
                .acquire_trace_filter_boost("invalid=verbose")
                .is_err()
        );
        assert!(
            service
                .acquire_trace_filter_boost(&"a".repeat(TRACE_FILTER_MAX_BYTES + 1))
                .is_err()
        );
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn nested_span_fields_propagate_into_log_hub_and_sse_stream() {
        use std::io::Read;

        let file = temporary_filter_file();
        let (service, dispatch) = LoggingService::for_test(file.clone(), "info,keeppeek=debug");
        let mut reader = service.stream(None, 10).unwrap();

        tracing::dispatcher::with_default(&dispatch, || {
            let outer = tracing::info_span!("camera", camera_id = "cam-1");
            let _outer_guard = outer.enter();
            let inner = tracing::info_span!("stream", stream = "main");
            let _inner_guard = inner.enter();
            tracing::info!(target: "keeppeek::rtsp", "connected");
        });
        service.close_streams();

        let snapshot = service.snapshot(None, 10);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].fields["camera_id"], "cam-1");
        assert_eq!(snapshot.entries[0].fields["stream"], "main");
        assert!(snapshot.entries[0].matches_query("cam-1"));

        let mut sse = String::new();
        reader.read_to_string(&mut sse).unwrap();
        assert!(sse.contains("\"camera_id\":\"cam-1\""));
        assert!(sse.contains("\"stream\":\"main\""));
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }

    #[test]
    fn registry_try_init_bridges_retina_log_records_into_log_hub() {
        let file = temporary_filter_file();
        let hub = LogHub::default();
        let (filter_layer, reload_handle) =
            reload::Layer::new(EnvFilter::try_new("info,retina=warn").unwrap());
        let init_result = Registry::default()
            .with(filter_layer)
            .with(LogCaptureLayer::new(hub.clone()))
            .try_init();
        if init_result.is_ok() {
            let service = LoggingService::new(
                hub.clone(),
                file.clone(),
                reload_handle,
                "info,retina=warn".to_owned(),
                None,
            );
            let span = tracing::info_span!("camera", camera_id = "cam-1", stream = "main");
            let _guard = span.enter();
            let _ = retina::codec::Depacketizer::new(
                "video",
                "h264",
                90_000,
                None,
                Some("sprop-parameter-sets=!!!invalid!!!"),
            );
            let _boost = service.acquire_trace_filter_boost("retina=trace").unwrap();
            let mut client = retina::client::core::RtspClient::new(
                retina::client::core::ClientOptions::default(),
            );
            let _ = client.handle_input(retina::client::core::Input::Command {
                time: retina::client::core::Time {
                    monotonic: std::time::Instant::now(),
                    wall: std::time::SystemTime::now(),
                },
                command: retina::client::core::Command::Describe {
                    url: url::Url::parse("rtsp://127.0.0.1:554/main").unwrap(),
                },
            });
            let entries = hub.snapshot(None, 20).entries;
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.target.starts_with("retina")
                        && entry
                            .fields
                            .get("camera_id")
                            .and_then(serde_json::Value::as_str)
                            == Some("cam-1")),
                "expected bridged retina log entry with span camera_id, got: {entries:?}"
            );
        }
        let _ = fs::remove_dir_all(file.path().parent().unwrap());
    }
}
