use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Serialize;
use tracing::info;
use tracing_subscriber::{
    EnvFilter, Registry,
    prelude::*,
    reload::{self, Handle},
    util::SubscriberInitExt,
};

use crate::config;

mod hub;
mod layer;
mod stream;

pub use hub::{
    LogBufferStats, LogEntry, LogHub, LogHubLimits, LogLevel, LogSnapshot, LogStreamError,
};
pub use layer::LogCaptureLayer;
pub use stream::LogStreamReader;

pub const DEFAULT_LOG_FILTER: &str = "info,keeppeek=debug";

#[derive(Clone)]
pub struct LoggingService {
    hub: LogHub,
    filter_file: LogFilterFile,
    reload_handle: Handle<EnvFilter, Registry>,
    active_filter: Arc<Mutex<String>>,
    filter_error: Arc<Mutex<Option<String>>>,
    update_lock: Arc<Mutex<()>>,
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

    Registry::default()
        .with(filter_layer)
        .with(tracing_subscriber::fmt::layer())
        .with(LogCaptureLayer::new(hub.clone()))
        .try_init()?;

    let service = LoggingService::new(
        hub,
        filter_file,
        reload_handle,
        initial_filter.directive,
        initial_filter.error,
    );
    if let Some(error) = service.filter_error() {
        tracing::warn!(%error, "using fallback log filter");
    }
    Ok(service)
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
            active_filter: Arc::new(Mutex::new(active_filter)),
            filter_error: Arc::new(Mutex::new(filter_error)),
            update_lock: Arc::new(Mutex::new(())),
        }
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
        let previous_file = std::fs::read(self.filter_file.path()).ok();
        self.filter_file.write_log_filter(log_filter)?;
        if let Err(error) = read_log_filter_file_and_reload(&self.filter_file, &self.reload_handle)
        {
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
            return Err(error);
        }

        *self
            .active_filter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = log_filter.to_owned();
        *self
            .filter_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        Ok(())
    }

    pub fn snapshot(&self, after: Option<u64>, limit: usize) -> LogSnapshot {
        self.hub.snapshot(after, limit)
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
}
