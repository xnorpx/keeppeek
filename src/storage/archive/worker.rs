//! Runs one bounded archive transfer outside camera ingest and writer completion.

use super::{
    filesystem::FilesystemArchiveTarget,
    settings::Settings,
    target::{ArchiveTarget, ObjectId, Work},
};
use crate::storage::catalog::{
    RecordingCatalogHandle,
    archive::{Admission, Binding, Job, Reply, Request},
};
use crate::storage::volumes::root::Root;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod cleanup;
#[cfg(test)]
mod tests;
mod transfer;

const PAGE_SIZE: u16 = 64;
const SCAN_INTERVAL: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct Health {
    pub available: bool,
    pub message: Option<&'static str>,
    pub probe_pending: bool,
}

#[derive(Debug)]
struct Active {
    id: String,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug)]
struct Shared {
    stopped: AtomicBool,
    health: Mutex<Health>,
    active: Mutex<Option<Active>>,
}

#[derive(Debug)]
enum Command {
    Wake,
    Probe { deadline: Instant },
}

#[derive(Clone)]
pub struct Handle {
    pub(super) settings: Arc<Settings>,
    pub(super) catalog: RecordingCatalogHandle,
    sender: SyncSender<Command>,
    shared: Arc<Shared>,
}

pub struct Worker {
    handle: Handle,
    thread: Option<JoinHandle<()>>,
}

struct Context {
    settings: Arc<Settings>,
    catalog: RecordingCatalogHandle,
    shared: Arc<Shared>,
}

struct Scan {
    after: String,
    candidates_after: String,
    pending: VecDeque<Job>,
    jobs_active: bool,
    candidates_active: bool,
    include_failed: bool,
    rescan: bool,
    bound: bool,
    next_periodic: Instant,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArchiveHandle")
            .field("health", &self.health())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArchiveWorker")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl Handle {
    pub(crate) fn target_id(&self) -> &str {
        &self.settings.target_id
    }

    pub(crate) fn enqueue(
        &self,
        recording_id: &str,
        kind: &str,
        job_id: &str,
    ) -> anyhow::Result<Job> {
        self.enabled()?;
        ObjectId::parse(job_id)?;
        anyhow::ensure!(
            !recording_id.is_empty() && recording_id.len() <= 256,
            "invalid archive source ID"
        );
        let job = admit(&self.settings, &self.catalog, recording_id, kind, job_id)?;
        self.scan()?;
        Ok(job)
    }

    pub(crate) fn get(&self, job_id: &str) -> anyhow::Result<Job> {
        ObjectId::parse(job_id)?;
        self.catalog.archive_job(job_id)
    }

    pub(crate) fn enqueue_export(
        &self,
        artifact_id: &str,
        archive_job_id: &str,
        original_export_job_id: &str,
        metadata_json: &str,
    ) -> anyhow::Result<Job> {
        self.enabled()?;
        ObjectId::parse(archive_job_id)?;
        let mut admission = admission(&self.settings, artifact_id, "export", archive_job_id);
        admission.export_job_id = Some(original_export_job_id.into());
        admission.export_metadata = Some(metadata_json.into());
        let job = enqueue_admission(&self.catalog, admission)?;
        self.scan()?;
        Ok(job)
    }

    pub(crate) fn list(&self, after: &str, limit: u16) -> anyhow::Result<Vec<Job>> {
        let Reply::Jobs(jobs) = self.catalog.archive(Request::List {
            after: after.into(),
            limit,
        })?
        else {
            anyhow::bail!("invalid archive list reply");
        };
        Ok(jobs)
    }

    pub(crate) fn retry(&self, job_id: &str) -> anyhow::Result<Job> {
        self.enabled()?;
        ObjectId::parse(job_id)?;
        self.catalog.archive(Request::Retry(job_id.into()))?;
        self.scan()?;
        self.get(job_id)
    }

    pub(crate) fn restore(&self, job_id: &str) -> anyhow::Result<Job> {
        self.request_action(job_id, "restore")
    }

    pub(crate) fn evict(&self, job_id: &str) -> anyhow::Result<Job> {
        self.request_action(job_id, "evict")
    }

    fn request_action(&self, job_id: &str, action: &str) -> anyhow::Result<Job> {
        self.enabled()?;
        ObjectId::parse(job_id)?;
        self.catalog.archive(Request::Action {
            id: job_id.into(),
            action: action.into(),
        })?;
        self.scan()?;
        self.get(job_id)
    }

    pub(crate) fn cancel(&self, job_id: &str) -> anyhow::Result<Job> {
        ObjectId::parse(job_id)?;
        self.catalog.archive(Request::Cancel(job_id.into()))?;
        if let Some(active) = self
            .shared
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("archive cancellation unavailable"))?
            .as_ref()
            && active.id == job_id
        {
            active.cancelled.store(true, Ordering::Release);
        }
        self.scan()?;
        self.get(job_id)
    }

    pub fn scan(&self) -> anyhow::Result<()> {
        match self.sender.try_send(Command::Wake) {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => {
                anyhow::bail!("archive worker stopped; job remains journaled")
            }
        }
    }

    pub fn request_probe(&self) -> anyhow::Result<()> {
        self.settings.validate()?;
        let mut health = self
            .shared
            .health
            .lock()
            .map_err(|_| anyhow::anyhow!("archive health is unavailable"))?;
        anyhow::ensure!(!health.probe_pending, "archive probe is already pending");
        self.sender
            .try_send(Command::Probe {
                deadline: Instant::now() + PROBE_TIMEOUT,
            })
            .map_err(|_| anyhow::anyhow!("archive worker is busy or unavailable"))?;
        health.probe_pending = true;
        Ok(())
    }

    pub fn health(&self) -> Health {
        self.shared.health.lock().map_or_else(
            |_| Health {
                available: false,
                message: Some("Archive health is unavailable"),
                probe_pending: false,
            },
            |health| health.clone(),
        )
    }

    fn enabled(&self) -> anyhow::Result<()> {
        self.settings.validate()?;
        anyhow::ensure!(self.settings.enabled, "archive is disabled");
        anyhow::ensure!(
            !self.shared.stopped.load(Ordering::Acquire),
            "archive worker stopped"
        );
        Ok(())
    }
}

impl Worker {
    pub fn start(settings: Settings, catalog: RecordingCatalogHandle) -> anyhow::Result<Self> {
        let settings = Arc::new(settings);
        let shared = Arc::new(Shared {
            stopped: AtomicBool::new(false),
            health: Mutex::new(Health {
                available: false,
                message: Some("Archive target is preparing"),
                probe_pending: false,
            }),
            active: Mutex::new(None),
        });
        let (sender, receiver) = mpsc::sync_channel(1);
        let context = Context {
            settings: settings.clone(),
            catalog: catalog.clone(),
            shared: shared.clone(),
        };
        let thread = thread::Builder::new()
            .name("archive-worker".into())
            .spawn(move || run(context, receiver))?;
        Ok(Self {
            handle: Handle {
                settings,
                catalog,
                sender,
                shared,
            },
            thread: Some(thread),
        })
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn shutdown(mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.handle.shared.stopped.store(true, Ordering::Release);
        let _ = self.handle.scan();
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("archive worker panicked"))?;
        }
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.stop().is_err() {
            tracing::error!("archive worker shutdown failed");
        }
    }
}

impl Context {
    fn health(&self, available: bool, message: Option<&'static str>) {
        if let Ok(mut health) = self.shared.health.lock() {
            health.available = available;
            health.message = message;
        }
    }

    fn bind(&self) -> anyhow::Result<()> {
        self.settings.validate()?;
        let root = Root::open(&self.settings.root)?;
        self.catalog.archive(Request::Bind(Binding {
            id: self.settings.target_id.clone(),
            root: self.settings.root.clone(),
            identity: root.identity().clone(),
        }))?;
        Ok(())
    }

    fn process(&self, job: Job, deadline: Instant) -> anyhow::Result<()> {
        let cancelled = Arc::new(AtomicBool::new(false));
        *self
            .shared
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("archive cancellation unavailable"))? = Some(Active {
            id: job.id.clone(),
            cancelled: cancelled.clone(),
        });
        let cancellation =
            || self.shared.stopped.load(Ordering::Acquire) || cancelled.load(Ordering::Acquire);
        let work = Work {
            deadline,
            cancelled: &cancellation,
        };
        let result = self.attempt(&job, &work);
        if let Ok(mut active) = self.shared.active.lock() {
            *active = None;
        }
        if result.is_err() && !cancellation() {
            let request = if job.cancelled {
                Request::CleanupFailed(job.id.clone())
            } else if job.action.is_empty() {
                Request::Fail(job.id.clone())
            } else {
                Request::ActionFailed { id: job.id.clone() }
            };
            if self.catalog.archive(request).is_err() {
                tracing::warn!(
                    job_id = job.id,
                    "archive failure state could not be committed; journal retained"
                );
            }
            self.health(
                false,
                Some("Archive operation failed; check the target and retry"),
            );
        }
        result
    }

    fn attempt(&self, job: &Job, work: &Work<'_>) -> anyhow::Result<()> {
        work.check()?;
        anyhow::ensure!(
            job.binding.id == self.settings.target_id && job.binding.root == self.settings.root,
            "archive job target is no longer configured"
        );
        if job.cancelled {
            return cleanup::execute(&self.catalog, job, work);
        } else if !job.action.is_empty() {
            let Reply::Job(Some(started)) = self.catalog.archive(Request::StartAction {
                id: job.id.clone(),
                maximum_attempts: self.settings.maximum_attempts,
            })?
            else {
                anyhow::bail!("invalid archive action reply");
            };
            let outcome = super::operations::execute(&self.catalog, &started, work)?;
            self.catalog
                .archive(Request::ActionComplete { id: job.id.clone() })?;
            if outcome == super::operations::Outcome::Unchanged {
                return Ok(());
            }
        } else if job.kind == "probe" {
            let outcome =
                transfer::probe(&self.catalog, job, self.settings.maximum_attempts, work)?;
            if outcome == transfer::ProbeOutcome::Unchanged {
                return Ok(());
            }
        } else {
            // An offline target must not exhaust the transfer retry budget.
            drop(FilesystemArchiveTarget::open(
                &job.binding.root,
                &job.binding.identity,
            )?);
            work.check()?;
            let Reply::Job(Some(started)) = self.catalog.archive(Request::Start {
                id: job.id.clone(),
                maximum_attempts: self.settings.maximum_attempts,
            })?
            else {
                anyhow::bail!("invalid archive start reply");
            };
            transfer::copy(&self.catalog, &started, work)?;
            if self.settings.evict_after_archive && job.kind == "recording" {
                self.catalog.archive(Request::Action {
                    id: job.id.clone(),
                    action: "evict".into(),
                })?;
            }
        }
        self.health(true, None);
        Ok(())
    }

    fn probe(&self, deadline: Instant) -> anyhow::Result<()> {
        anyhow::ensure!(Instant::now() < deadline, "archive probe deadline expired");
        self.bind()?;
        let mut job = admit(
            &self.settings,
            &self.catalog,
            &format!("probe:{}", self.settings.target_id),
            "probe",
            &ObjectId::random().to_string(),
        )?;
        if job.state == "failed" {
            self.catalog.archive(Request::Retry(job.id.clone()))?;
            job = self.catalog.archive_job(&job.id)?;
        }
        self.process(job, deadline)
    }
}

fn admit(
    settings: &Settings,
    catalog: &RecordingCatalogHandle,
    recording_id: &str,
    kind: &str,
    job_id: &str,
) -> anyhow::Result<Job> {
    enqueue_admission(catalog, admission(settings, recording_id, kind, job_id))
}

fn admission(settings: &Settings, recording_id: &str, kind: &str, job_id: &str) -> Admission {
    Admission {
        job_id: job_id.into(),
        recording_id: recording_id.into(),
        kind: kind.into(),
        target: settings.target_id.clone(),
        object_limit: settings.maximum_object_bytes,
        count_limit: settings.maximum_pending_count,
        bytes_limit: settings.maximum_pending_bytes,
        export_job_id: None,
        export_metadata: None,
    }
}

fn enqueue_admission(
    catalog: &RecordingCatalogHandle,
    admission: Admission,
) -> anyhow::Result<Job> {
    let Reply::Job(Some(job)) = catalog.archive(Request::Enqueue(admission))? else {
        anyhow::bail!("invalid archive admission reply");
    };
    Ok(job)
}

fn run(context: Context, receiver: Receiver<Command>) {
    let mut scan = Scan::new();
    let configured = context.settings.validate().is_ok();
    // The worker runs until shutdown; each turn handles one command and one job or catalog page.
    while !context.shared.stopped.load(Ordering::Acquire) {
        if let Ok(command) = receiver.try_recv() {
            dispatch(&context, &mut scan, command);
        }
        if !scan.active() && Instant::now() >= scan.next_periodic {
            scan.begin(true);
        }
        let worked = if context.settings.enabled && configured {
            match scan.turn(&context) {
                Ok(worked) => worked,
                Err(_) => {
                    context.health(
                        false,
                        Some("Archive target is unavailable; check its configuration"),
                    );
                    scan.defer();
                    false
                }
            }
        } else {
            context.health(
                false,
                Some(if configured {
                    "Archive is disabled"
                } else {
                    "Archive configuration is invalid"
                }),
            );
            false
        };
        if !worked {
            match receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(command) => dispatch(&context, &mut scan, command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

fn dispatch(context: &Context, scan: &mut Scan, command: Command) {
    match command {
        Command::Wake => scan.wake(),
        Command::Probe { deadline } => {
            let result = context.probe(deadline);
            if result.is_err() {
                context.health(
                    false,
                    Some("Archive probe failed; check the target and retry"),
                );
            }
            if let Ok(mut health) = context.shared.health.lock() {
                health.probe_pending = false;
            }
            scan.wake();
        }
    }
}

impl Scan {
    fn new() -> Self {
        Self {
            after: String::new(),
            candidates_after: String::new(),
            pending: VecDeque::new(),
            jobs_active: true,
            candidates_active: true,
            include_failed: false,
            rescan: false,
            bound: false,
            next_periodic: Instant::now() + SCAN_INTERVAL,
        }
    }

    fn active(&self) -> bool {
        self.jobs_active || self.candidates_active || !self.pending.is_empty()
    }

    fn begin(&mut self, periodic: bool) {
        self.after.clear();
        self.candidates_after.clear();
        self.pending.clear();
        self.jobs_active = true;
        self.candidates_active = true;
        self.include_failed = periodic;
        self.rescan = false;
        self.bound = false;
        if periodic {
            self.next_periodic = Instant::now() + SCAN_INTERVAL;
        }
    }

    fn wake(&mut self) {
        if self.active() {
            self.rescan = true;
        } else {
            self.begin(Instant::now() >= self.next_periodic);
        }
    }

    fn defer(&mut self) {
        self.pending.clear();
        self.jobs_active = false;
        self.candidates_active = false;
        self.rescan = false;
        self.bound = false;
        self.next_periodic = Instant::now() + SCAN_INTERVAL;
    }

    fn turn(&mut self, context: &Context) -> anyhow::Result<bool> {
        if !self.active() {
            if self.rescan {
                self.begin(Instant::now() >= self.next_periodic);
            } else {
                return Ok(false);
            }
        }
        if !self.bound {
            if context.bind().is_err() {
                context.health(
                    false,
                    Some("Archive target is unavailable; check its configuration"),
                );
            }
            self.bound = true;
        }
        if let Some(job) = self.pending.pop_front() {
            let attempts = if job.action.is_empty() {
                job.attempts
            } else {
                job.action_attempts
            };
            let probe_cleanup =
                job.kind == "probe" && matches!(job.state.as_str(), "ready" | "deleted");
            let failed = job.state == "failed"
                || ((!job.action.is_empty() || job.cancelled) && job.error.is_some());
            if (attempts < context.settings.maximum_attempts || probe_cleanup || job.cancelled)
                && (!failed || self.include_failed)
            {
                let _ = context.process(
                    job,
                    Instant::now() + Duration::from_secs(context.settings.operation_timeout_secs),
                );
            }
            return Ok(true);
        }
        if self.jobs_active {
            let Reply::Jobs(jobs) = context.catalog.archive(Request::Pending {
                after: self.after.clone(),
                limit: PAGE_SIZE,
            })?
            else {
                anyhow::bail!("invalid archive pending reply");
            };
            self.jobs_active = !jobs.is_empty();
            if let Some(last) = jobs.last() {
                self.after.clone_from(&last.id);
            }
            self.pending = jobs.into();
            return Ok(true);
        }
        if self.candidates_active {
            return self.candidates(context);
        }
        if self.rescan {
            self.begin(Instant::now() >= self.next_periodic);
        }
        Ok(self.active())
    }

    fn candidates(&mut self, context: &Context) -> anyhow::Result<bool> {
        if !context.settings.automatic {
            self.candidates_active = false;
            return Ok(true);
        }
        let now =
            i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000)?;
        let age = i64::try_from(
            context
                .settings
                .minimum_age_secs
                .checked_mul(1000)
                .ok_or_else(|| anyhow::anyhow!("archive age overflow"))?,
        )?;
        let Reply::Candidates(candidates) = context.catalog.archive(Request::Candidates {
            after: self.candidates_after.clone(),
            before_ms: now.saturating_sub(age),
            limit: PAGE_SIZE,
        })?
        else {
            anyhow::bail!("invalid archive candidate reply");
        };
        self.candidates_active = candidates.len() == usize::from(PAGE_SIZE);
        if let Some((last, _)) = candidates.last() {
            self.candidates_after.clone_from(last);
        }
        let mut admitted = false;
        for (id, source) in candidates {
            if context.settings.source_ids.is_empty()
                || source.is_some_and(|source| context.settings.source_ids.contains(&source))
            {
                match admit(
                    &context.settings,
                    &context.catalog,
                    &id,
                    "recording",
                    &ObjectId::random().to_string(),
                ) {
                    Ok(_) => admitted = true,
                    Err(_) => context.health(
                        false,
                        Some(
                            "Archive admission failed; check queue limits and source availability",
                        ),
                    ),
                }
            }
        }
        if admitted {
            self.jobs_active = true;
            self.after.clear();
        }
        Ok(true)
    }
}
