use super::{RecordingCatalogHandle, RecordingHealthRegistry, Scan, StorageConfig};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
pub(in crate::storage::engine) struct Background {
    sender: Option<mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    published: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    finished: mpsc::Receiver<()>,
}

impl Background {
    pub(in crate::storage::engine) fn start(
        config: StorageConfig,
        catalog: Option<RecordingCatalogHandle>,
        health: RecordingHealthRegistry,
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let published = Arc::new(AtomicBool::new(false));
        let updated = published.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::clone(&stop);
        let (done, finished) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("recording-relocation".into())
            .spawn(move || {
                let _done = done;
                run_background(
                    receiver,
                    &config,
                    catalog.as_ref(),
                    &health,
                    &updated,
                    cancellation,
                );
            })
            .expect("failed to spawn recording relocation worker");
        Self {
            sender: Some(sender),
            thread: Some(thread),
            published,
            stop,
            finished,
        }
    }

    pub(in crate::storage::engine) fn wake(&self) {
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(());
        }
    }

    pub(in crate::storage::engine) fn take_published(&self) -> bool {
        self.published.swap(false, Ordering::AcqRel)
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            // File-system drivers may block inside even one read or fsync. The journal
            // owns the bytes and the in-process attempt guard fences a later startup.
            if self.finished.recv_timeout(Duration::from_millis(250))
                == Err(mpsc::RecvTimeoutError::Timeout)
            {
                tracing::warn!("recording relocation I/O still pending at shutdown");
            } else if thread.join().is_err() {
                tracing::error!("recording relocation worker panicked");
            }
        }
    }
}

fn run_background(
    rx: mpsc::Receiver<()>,
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    health: &RecordingHealthRegistry,
    published: &AtomicBool,
    stop: Arc<AtomicBool>,
) {
    let mut scan = Scan {
        stop: Arc::clone(&stop),
        ..Scan::new(true)
    };
    let mut dirty = false;
    while !stop.load(Ordering::Acquire) {
        let delay = scan.step(config, catalog, health, Instant::now());
        if scan.take_published() {
            published.store(true, Ordering::Release);
        }
        // New finalizations may be in a directory already visited. Coalesce all of them
        // into one further pass while this worker is still running.
        let complete = scan.pass_finished;
        if complete && dirty {
            dirty = false;
            scan.due = Some(Instant::now());
            continue;
        }
        if complete && delay.is_none() && config.medium_term_path == config.long_term_path {
            break;
        }
        let received = delay.map_or_else(
            || rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            |delay| rx.recv_timeout(delay),
        );
        match received {
            Ok(()) if complete => scan.due = Some(Instant::now()),
            Ok(()) => dirty = true,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_worker_coalesces_capture_wakeups_without_waiting_for_the_copy() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::channel();
        let (done, finished) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _done = done;
            gate.recv().unwrap();
            assert_eq!(receiver.into_iter().count(), 1);
        });
        let worker = Background {
            sender: Some(sender),
            thread: Some(thread),
            published: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            finished,
        };
        let root = std::env::temp_dir().join(format!("keeppeek-blocked-{}", uuid::Uuid::new_v4()));
        let config = StorageConfig {
            medium_term_path: root.join("capture"),
            long_term_path: root.join("archive"),
            ..StorageConfig::default()
        };
        let source = config
            .medium_term_path
            .join("front/main/2026-11-22/01/0000001.mp4");
        let mut capture = crate::storage::engine::WriterWorker::new(
            config,
            crate::storage::engine::RecordingDemand::new(std::time::Duration::ZERO),
            None,
        );
        capture.relocator = Some(worker);
        // These completions enter the same handoff as a rotated or final event recording.
        // Even a missing source would produce an I/O error if the capture thread tried to copy it.
        for _ in 0..4096 {
            assert_eq!(
                capture
                    .move_to_long_term("front/main", &source, "recording")
                    .unwrap(),
                source
            );
        }
        let thread = capture.relocator.as_mut().unwrap().thread.take().unwrap();
        release.send(()).unwrap();
        drop(capture);
        thread.join().unwrap();
    }
    #[test]
    fn stalled_filesystem_call_leaves_shutdown_and_releases_its_attempt_when_it_returns() {
        let (sender, _) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let (release, gate) = mpsc::channel();
        let (exited, exit) = mpsc::channel();
        let (done, finished) = mpsc::channel::<()>();
        let cancellation = stop.clone();
        let thread = std::thread::spawn(move || {
            let _done = done;
            gate.recv().unwrap();
            exited.send(cancellation.load(Ordering::Acquire)).unwrap();
        });
        let worker = Background {
            sender: Some(sender),
            thread: Some(thread),
            published: Arc::default(),
            stop,
            finished,
        };
        let (returned, result) = mpsc::channel();
        let service = std::thread::spawn(move || {
            drop(worker);
            returned.send(()).unwrap();
        });
        let shutdown = result.recv_timeout(Duration::from_secs(5));
        release.send(()).unwrap();
        assert!(exit.recv_timeout(Duration::from_secs(5)).unwrap());
        service.join().unwrap();
        assert!(
            shutdown.is_ok(),
            "shutdown waited on the stalled filesystem"
        );
    }
}
