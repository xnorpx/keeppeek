use super::{RecordingCatalogHandle, RecordingHealthRegistry, Scan, StorageConfig};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};
pub(in crate::storage::engine) struct Background {
    sender: Option<mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    published: Arc<AtomicBool>,
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
        let thread = std::thread::Builder::new()
            .name("recording-relocation".into())
            .spawn(move || run_background(receiver, &config, catalog.as_ref(), &health, &updated))
            .expect("failed to spawn recording relocation worker");
        Self {
            sender: Some(sender),
            thread: Some(thread),
            published,
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
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_background(
    rx: mpsc::Receiver<()>,
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    health: &RecordingHealthRegistry,
    published: &AtomicBool,
) {
    let mut scan = Scan::new(true);
    let mut dirty = false;
    let mut stopping = false;
    loop {
        let delay = scan.step(config, catalog, health, Instant::now());
        if scan.published {
            published.store(true, Ordering::Release);
            scan.published = false;
        }
        // New finalizations may be in a directory already visited. Coalesce all of them
        // into one further complete pass, and finish the last pass on orderly shutdown.
        let complete = scan.pass_finished;
        if complete && dirty {
            dirty = false;
            scan.due = Some(Instant::now());
            continue;
        }
        if stopping {
            if complete {
                break;
            }
            scan.due = Some(Instant::now());
            continue;
        }
        let received = delay.map_or_else(
            || rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            |delay| rx.recv_timeout(delay),
        );
        match received {
            Ok(()) if complete => scan.due = Some(Instant::now()),
            Ok(()) => dirty = true,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                stopping = true;
                dirty = true;
                scan.due = Some(Instant::now());
            }
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
        let thread = std::thread::spawn(move || {
            gate.recv().unwrap();
            assert_eq!(receiver.into_iter().count(), 1);
        });
        let worker = Background {
            sender: Some(sender),
            thread: Some(thread),
            published: Arc::new(AtomicBool::new(false)),
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
        release.send(()).unwrap();
        drop(capture);
    }
}
