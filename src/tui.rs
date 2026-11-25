//! Foreground-only, read-only terminal dashboard.
#[cfg(test)]
mod benchmark;
mod mode;
mod render;
mod snapshot;
mod terminal;
mod text;
use crate::{health::ServerHealthResponse, logging::LogHub, shutdown::Shutdown};
pub use mode::DashboardMode;
use mode::Presentation;
use snapshot::Snapshot;
use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

type Latest = Arc<Mutex<Observation>>;

#[derive(Clone, Default)]
struct Observation {
    snapshot: Option<Arc<Snapshot>>,
    failed_at: Option<Instant>,
    unread_snapshot: bool,
}

impl Observation {
    fn update(&mut self, snapshot: Option<Arc<Snapshot>>, now: Instant) -> bool {
        if let Some(snapshot) = snapshot {
            let replaced_unread = self.unread_snapshot;
            self.snapshot = Some(snapshot);
            self.unread_snapshot = true;
            self.failed_at = None;
            replaced_unread
        } else {
            self.failed_at.get_or_insert(now);
            false
        }
    }

    fn read_for_render(&mut self) -> Self {
        self.unread_snapshot = false;
        self.clone()
    }
}

struct CancelOnDrop(Shutdown);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(crate) struct Dashboard {
    stop: Shutdown,
    finished: mpsc::Receiver<()>,
}

impl Dashboard {
    pub(crate) fn start(
        mode: DashboardMode,
        shutdown: Shutdown,
        collect: impl Fn() -> Option<ServerHealthResponse> + Send + 'static,
        redactor: LogHub,
    ) -> Option<Self> {
        match mode.presentation() {
            Presentation::Plain => return None,
            Presentation::Unavailable => {
                eprintln!(
                    "KeepPeek dashboard requires an interactive terminal. Continuing with plain logs."
                );
                return None;
            }
            Presentation::Dashboard => {}
        }
        let stop = Shutdown::new();
        let presentation_stop = stop.clone();
        let (done, finished) = mpsc::sync_channel(1);
        let owner = thread::Builder::new()
            .name("terminal-dashboard".into())
            .spawn(move || {
                let _collector_lifetime = CancelOnDrop(presentation_stop.clone());
                let latest = Latest::default();
                let writer = latest.clone();
                let collector_stop = presentation_stop.clone();
                let server_stop = shutdown.clone();
                let result = terminal::present(&latest, &presentation_stop, &shutdown, move || {
                    thread::Builder::new()
                        .name("terminal-health".into())
                        .spawn(move || {
                            run_collector(
                                writer,
                                collector_stop,
                                server_stop,
                                collect,
                                redactor,
                                Duration::from_secs(2),
                            );
                        })
                        .map(|_| ())
                });
                presentation_stop.cancel();
                if let Err(error) = result {
                    eprintln!(
                        "KeepPeek dashboard closed ({:?}). Continuing with plain logs.",
                        error.kind()
                    );
                }
                let _ = done.send(());
            });
        if let Err(error) = owner {
            eprintln!(
                "KeepPeek dashboard could not start ({:?}). Continuing with plain logs.",
                error.kind()
            );
            return None;
        }
        Some(Self { stop, finished })
    }
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        self.stop.cancel();
        if matches!(
            self.finished.recv_timeout(Duration::from_millis(500)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            terminal::restore_before_exit();
        }
    }
}

fn run_collector(
    writer: Latest,
    collector_stop: Shutdown,
    server_stop: Shutdown,
    collect: impl Fn() -> Option<ServerHealthResponse>,
    redactor: LogHub,
    cadence: Duration,
) {
    let _screen_lifetime = CancelOnDrop(collector_stop.clone());
    while !collector_stop.is_cancelled() && !server_stop.is_cancelled() {
        let snapshot = collect().map(|health| Arc::new(Snapshot::from_health(health, &redactor)));
        writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .update(snapshot, Instant::now());
        if collector_stop.wait_timeout(cadence) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn slow_collector_stays_single_flight_skips_ticks_and_recovers_after_failure() {
        let latest = Latest::default();
        let stop = Shutdown::new();
        let server = Shutdown::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));

        let c_calls = calls.clone();
        let c_max = max_in_flight.clone();
        let c_latest = latest.clone();
        let handle = thread::spawn(move || {
            run_collector(
                c_latest,
                stop.clone(),
                server,
                move || {
                    let active = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
                    c_max.fetch_max(active, Ordering::AcqRel);
                    let attempt = c_calls.fetch_add(1, Ordering::AcqRel) + 1;
                    thread::sleep(Duration::from_millis(45));
                    in_flight.fetch_sub(1, Ordering::AcqRel);
                    if attempt == 2 {
                        None
                    } else {
                        if attempt >= 3 {
                            stop.cancel();
                        }
                        Some(snapshot::test_server_health(
                            Vec::new(),
                            Vec::new(),
                            attempt as u64,
                        ))
                    }
                },
                LogHub::default(),
                Duration::from_millis(10),
            );
        });
        handle.join().unwrap();
        assert_eq!(max_in_flight.load(Ordering::Acquire), 1);
        assert_eq!(calls.load(Ordering::Acquire), 3);
        let obs = latest.lock().unwrap().read_for_render();
        assert_eq!(obs.snapshot.as_ref().unwrap().uptime_s, 3);
        assert!(obs.failed_at.is_none());
    }
}
