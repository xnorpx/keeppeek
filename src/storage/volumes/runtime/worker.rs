//! Runs bounded durable move jobs outside the recording writer.

use super::Manager;
use crate::storage::{
    catalog::locations::{Object, Reply, Request, moves::Page},
    volumes::PlacementRequest,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const QUEUE_CAPACITY: usize = 64;
const SCAN_LIMIT: usize = 4_096;
const SCAN_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Handle {
    sender: SyncSender<String>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MoveHandle")
    }
}

impl Handle {
    /// Reserves the move durably before issuing a bounded worker wakeup.
    ///
    /// # Errors
    /// Rejects unavailable placement or a stopped worker. Queue overflow remains journaled.
    pub fn schedule(
        &self,
        manager: &Manager,
        object: Object,
        request: &PlacementRequest<'_>,
        groups: &[&str],
    ) -> anyhow::Result<bool> {
        let id = uuid::Uuid::new_v4().to_string();
        if !manager.schedule_move(&id, object, request, groups)? {
            return Ok(false);
        }
        match self.sender.try_send(id) {
            Ok(()) => {}
            Err(TrySendError::Full(id)) => tracing::debug!(
                job_id = id,
                "move wakeup queue full; journal scan will recover it"
            ),
            Err(TrySendError::Disconnected(_)) => {
                anyhow::bail!("move worker stopped; admitted job remains journaled")
            }
        }
        Ok(true)
    }
}

pub struct Worker {
    handle: Handle,
    cancelled: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MoveWorker")
    }
}

impl Worker {
    /// Starts one worker; the thread owns no sender and cannot keep its queue alive.
    ///
    /// # Errors
    /// Returns an error if the operating system refuses the thread.
    pub fn start(manager: Manager) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::clone(&cancelled);
        let thread = thread::Builder::new()
            .name("volume-mover".into())
            .spawn(move || run(manager, receiver, &cancellation))?;
        Ok(Self {
            handle: Handle { sender },
            cancelled,
            thread: Some(thread),
        })
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    /// Cancels work and joins the actual worker before its catalog can close.
    ///
    /// # Errors
    /// Returns an error if the worker panicked.
    pub fn shutdown(mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.thread.take() {
            worker.thread().unpark();
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("volume move worker panicked"))?;
        }
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::error!(%error, "volume move worker shutdown failed");
        }
    }
}

fn run(manager: Manager, receiver: Receiver<String>, cancelled: &AtomicBool) {
    let mut scan = Scan::new();
    // This worker runs until shutdown; each turn handles one wakeup and one journal item.
    while !cancelled.load(Ordering::Acquire) {
        let mut worked = false;
        if let Ok(id) = receiver.try_recv() {
            process(&manager, &id, cancelled);
            worked = true;
        }
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        match scan.next(&manager) {
            Ok(Some(id)) => {
                process(&manager, &id, cancelled);
                worked = true;
            }
            Ok(None) => {}
            Err(error) => {
                scan.defer();
                tracing::warn!(%error, "volume move journal scan failed");
            }
        }
        if !worked {
            match receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(id) => process(&manager, &id, cancelled),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

fn process(manager: &Manager, id: &str, cancelled: &AtomicBool) {
    for attempt in 0..3 {
        if cancelled.load(Ordering::Acquire) {
            return;
        }
        match execute(manager, id, cancelled) {
            Ok(()) => return,
            Err(_) if cancelled.load(Ordering::Acquire) => return,
            Err(error) => {
                tracing::warn!(job_id = id, attempt, %error, "volume move attempt failed; journal retained");
                if attempt < 2 {
                    thread::park_timeout(Duration::from_secs(1_u64 << attempt));
                }
            }
        }
    }
}

fn execute(manager: &Manager, id: &str, cancelled: &AtomicBool) -> anyhow::Result<()> {
    let Reply::Move(job) = manager
        .inner
        .catalog
        .volume_location(Request::Move(id.to_owned()))?
    else {
        anyhow::bail!("invalid move journal reply");
    };
    if job.cancellation_requested {
        return manager.finish_cancelled_move(id);
    }
    match job.phase.as_str() {
        "reserved" | "verified" | "file_published" => {
            manager.resume_move(id, || cancelled.load(Ordering::Acquire))?;
            if !cancelled.load(Ordering::Acquire) {
                manager.retire_move(id)?;
            }
        }
        "published" | "retiring" | "complete" => {
            manager.retire_move(id)?;
        }
        "cancelled" => {}
        _ => anyhow::bail!("unknown move phase"),
    }
    Ok(())
}

struct Scan {
    after: Option<String>,
    pending: VecDeque<String>,
    seen: usize,
    next: Instant,
    active: bool,
}

impl Scan {
    fn new() -> Self {
        Self {
            after: None,
            pending: VecDeque::new(),
            seen: 0,
            next: Instant::now(),
            active: false,
        }
    }

    fn defer(&mut self) {
        self.active = false;
        self.pending.clear();
        self.next = Instant::now() + SCAN_INTERVAL;
    }

    fn next(&mut self, manager: &Manager) -> anyhow::Result<Option<String>> {
        if !self.active {
            if Instant::now() < self.next {
                return Ok(None);
            }
            manager.recover_roots()?;
            self.after = None;
            self.seen = 0;
            self.active = true;
        }
        if let Some(id) = self.pending.pop_front() {
            return Ok(Some(id));
        }
        if self.seen >= SCAN_LIMIT {
            self.defer();
            return Ok(None);
        }
        let Reply::Moves(jobs) = manager.inner.catalog.volume_location(Request::Moves(Page {
            after: self.after.clone(),
            limit: QUEUE_CAPACITY as u16,
            include_terminal: false,
        }))?
        else {
            anyhow::bail!("invalid move scan reply");
        };
        anyhow::ensure!(
            jobs.len() <= QUEUE_CAPACITY,
            "move scan exceeded its page budget"
        );
        if jobs.is_empty() {
            self.defer();
            return Ok(None);
        }
        self.after = jobs.last().map(|job| job.id.clone());
        self.seen += jobs.len();
        self.pending.extend(jobs.into_iter().map(|job| job.id));
        Ok(self.pending.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::super::movement_tests::fixture;
    use super::*;

    #[test]
    fn journal_scan_recovers_jobs_without_any_queue_wakeup() -> anyhow::Result<()> {
        let fixture = fixture()?;
        let mut scan = Scan::new();
        assert_eq!(scan.next(&fixture.manager)?, Some(fixture.job_id));
        assert_eq!(scan.next(&fixture.manager)?, None);
        assert!(!scan.active);
        assert!(scan.next > Instant::now());
        fixture.catalog.shutdown();
        Ok(())
    }

    #[test]
    fn startup_worker_publishes_durable_job_and_joins_before_catalog_shutdown() -> anyhow::Result<()>
    {
        let fixture = fixture()?;
        let worker = Worker::start(fixture.manager.clone())?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let Reply::Location(Some(location)) = fixture
                .catalog
                .handle()
                .volume_location(Request::Lookup(fixture.source.object.clone()))?
            else {
                anyhow::bail!("recording location disappeared");
            };
            if location.volume == "secondary" {
                break;
            }
            anyhow::ensure!(Instant::now() < deadline, "background move did not publish");
            thread::sleep(Duration::from_millis(10));
        }
        let stopped = Arc::clone(&worker.cancelled);
        worker.shutdown()?;
        assert!(stopped.load(Ordering::Acquire));
        let _lease = fixture
            .catalog
            .handle()
            .claim_volume_move(&fixture.job_id)?;
        assert!(fixture.destination.path.exists());
        fixture.catalog.shutdown();
        Ok(())
    }

    #[test]
    fn cancelled_job_is_not_started_by_the_background_worker() -> anyhow::Result<()> {
        let fixture = fixture()?;
        fixture
            .catalog
            .handle()
            .volume_location(Request::AdvanceMove(
                crate::storage::catalog::locations::moves::Step::Cancel(fixture.job_id.clone()),
            ))?;
        execute(&fixture.manager, &fixture.job_id, &AtomicBool::new(false))?;
        assert!(!fixture.destination.path.exists());
        assert!(!fixture.destination.path.with_extension("tmp").exists());
        assert!(fixture.source_path.exists());
        fixture.catalog.shutdown();
        Ok(())
    }
}
