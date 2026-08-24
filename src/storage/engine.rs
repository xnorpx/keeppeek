use crate::{
    cameras::CameraRecordingMode,
    config::StorageToml,
    storage::{
        catalog::RecordingCatalogHandle, demand::RecordingDemand,
        identity::RecordingStreamIdentity, long_term::LongTermStore, medium_term::MediumTermWriter,
        segment::RecordingFrame, short_term::ShortTermBuffer,
    },
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock, mpsc},
    time::{Duration, Instant},
};

const DEMAND_INACTIVITY_GRACE: Duration = Duration::from_secs(30);
const MEBIBYTE_BYTES: u64 = 1_048_576;
const GIBIBYTE_BYTES: u64 = 1_073_741_824;

#[derive(Clone)]
pub struct StorageConfig {
    pub medium_term_path: PathBuf,
    pub long_term_path: PathBuf,
    pub recording_catalog_path: PathBuf,
    pub event_thumbnail_path: PathBuf,
    pub event_thumbnail_max_bytes: u64,
    pub short_term_duration: Duration,
    pub medium_term_duration: Duration,
    pub flush_interval: Duration,
    pub write_buffer_bytes: usize,
    pub long_term_max_bytes: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self::from_toml(&StorageToml::default())
    }
}

impl StorageConfig {
    pub fn from_toml(toml: &StorageToml) -> Self {
        let default_root = crate::config::config_dir().join("recordings");
        let medium_term_path = toml
            .medium_term_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| default_root.clone());
        let long_term_path = toml
            .long_term_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or(default_root);
        let recording_catalog_path = toml
            .recording_catalog_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| long_term_path.join("recordings.db"));
        let event_thumbnail_path = toml
            .event_thumbnail_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| long_term_path.join(".event-thumbnails"));
        Self {
            medium_term_path,
            long_term_path,
            recording_catalog_path,
            event_thumbnail_path,
            event_thumbnail_max_bytes: toml.event_thumbnail_max_mb.saturating_mul(MEBIBYTE_BYTES),
            short_term_duration: Duration::from_secs(toml.short_term_secs),
            medium_term_duration: Duration::from_secs(toml.medium_term_secs),
            flush_interval: Duration::from_secs(toml.flush_interval_secs),
            write_buffer_bytes: toml.write_buffer_bytes,
            long_term_max_bytes: toml.long_term_max_gb.saturating_mul(GIBIBYTE_BYTES),
        }
    }

    const fn is_direct_write(&self) -> bool {
        self.short_term_duration.is_zero() && self.flush_interval.is_zero()
    }
}

enum Command {
    Ingest {
        identity: RecordingStreamIdentity,
        frame: RecordingFrame,
    },
    FlushAll,
    Shutdown,
}

struct CameraPipeline {
    identity: RecordingStreamIdentity,
    short_term: ShortTermBuffer,
    medium_term: Option<MediumTermWriter>,
    last_flush: Instant,
}

#[derive(Debug, Clone, Copy)]
struct CameraRecordingPolicy {
    mode: CameraRecordingMode,
    event_duration: Duration,
    main_until: Option<Instant>,
    event_main_state: EventMainState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum EventMainState {
    #[default]
    Idle,
    WaitingForKeyframe,
    Recording,
}

enum AdmissionDecision {
    Record,
    RecordAs(&'static str),
    Ignore,
}

#[derive(Clone, Default)]
struct RecordingAdmission {
    policies: Arc<RwLock<HashMap<String, CameraRecordingPolicy>>>,
}

impl RecordingAdmission {
    fn configure(&self, camera_id: &str, mode: CameraRecordingMode, event_duration: Duration) {
        self.policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                camera_id.to_owned(),
                CameraRecordingPolicy {
                    mode,
                    event_duration,
                    main_until: None,
                    event_main_state: EventMainState::Idle,
                },
            );
    }

    fn note_event_at(&self, camera_id: &str, now: Instant) {
        let mut policies = self
            .policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(policy) = policies.get_mut(camera_id) else {
            return;
        };
        if policy.mode == CameraRecordingMode::EventBoost {
            policy.main_until = now.checked_add(policy.event_duration);
            if policy.event_main_state == EventMainState::Idle {
                policy.event_main_state = EventMainState::WaitingForKeyframe;
            }
        }
    }

    #[cfg(test)]
    fn decide_at(
        &self,
        camera_id: &str,
        stream_id: &str,
        is_video: bool,
        is_video_keyframe: bool,
        now: Instant,
    ) -> AdmissionDecision {
        let mut policies = self
            .policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let policy =
            policies
                .entry(camera_id.to_owned())
                .or_insert_with(|| CameraRecordingPolicy {
                    mode: CameraRecordingMode::default(),
                    event_duration: Duration::from_secs(60),
                    main_until: None,
                    event_main_state: EventMainState::Idle,
                });
        Self::decide(policy, stream_id, is_video, is_video_keyframe, now)
    }

    fn ingest_at(
        &self,
        tx: &mpsc::Sender<Command>,
        identity: RecordingStreamIdentity,
        frame: RecordingFrame,
        now: Instant,
    ) {
        self.ingest_with_hook_at(tx, identity, frame, now, || {});
    }

    fn ingest_with_hook_at(
        &self,
        tx: &mpsc::Sender<Command>,
        mut identity: RecordingStreamIdentity,
        mut frame: RecordingFrame,
        now: Instant,
        before_send: impl FnOnce(),
    ) {
        let mut policies = self
            .policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let policy = policies
            .entry(identity.source_id.clone())
            .or_insert_with(|| CameraRecordingPolicy {
                mode: CameraRecordingMode::default(),
                event_duration: Duration::from_secs(60),
                main_until: None,
                event_main_state: EventMainState::Idle,
            });
        let decision = Self::decide(
            policy,
            &identity.stream_id,
            frame.frame.is_video(),
            frame.is_video_keyframe(),
            now,
        );
        before_send();
        match decision {
            AdmissionDecision::Record => {
                let _ = tx.send(Command::Ingest { identity, frame });
            }
            AdmissionDecision::RecordAs(stream_id) => {
                frame.timestamp = None;
                identity = identity.with_recording_stream(stream_id);
                let _ = tx.send(Command::Ingest { identity, frame });
            }
            AdmissionDecision::Ignore => {}
        }
    }

    fn decide(
        policy: &mut CameraRecordingPolicy,
        stream_id: &str,
        is_video: bool,
        is_video_keyframe: bool,
        now: Instant,
    ) -> AdmissionDecision {
        match (policy.mode, stream_id) {
            (CameraRecordingMode::Sub, "sub")
            | (CameraRecordingMode::Main, "main")
            | (CameraRecordingMode::Both, "main" | "sub") => AdmissionDecision::Record,
            (CameraRecordingMode::EventBoost, "main" | "sub") if !is_video => {
                let preferred = if policy.event_main_state == EventMainState::Recording {
                    "main"
                } else {
                    "sub"
                };
                if stream_id == preferred {
                    AdmissionDecision::RecordAs("sub")
                } else {
                    AdmissionDecision::Ignore
                }
            }
            (CameraRecordingMode::EventBoost, "main" | "sub") => {
                if policy.event_main_state == EventMainState::WaitingForKeyframe
                    && policy.main_until.is_none_or(|deadline| now >= deadline)
                {
                    policy.event_main_state = EventMainState::Idle;
                    policy.main_until = None;
                }
                match policy.event_main_state {
                    EventMainState::Idle if stream_id == "sub" => {
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::WaitingForKeyframe if stream_id == "sub" => {
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::WaitingForKeyframe
                        if stream_id == "main" && is_video_keyframe =>
                    {
                        policy.event_main_state = EventMainState::Recording;
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::Recording
                        if policy.main_until.is_some_and(|deadline| now < deadline)
                            && stream_id == "main" =>
                    {
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::Recording
                        if stream_id == "sub"
                            && is_video_keyframe
                            && policy.main_until.is_none_or(|deadline| now >= deadline) =>
                    {
                        policy.event_main_state = EventMainState::Idle;
                        policy.main_until = None;
                        AdmissionDecision::RecordAs("sub")
                    }
                    EventMainState::Recording if stream_id == "main" => {
                        AdmissionDecision::RecordAs("sub")
                    }
                    _ => AdmissionDecision::Ignore,
                }
            }
            _ => AdmissionDecision::Ignore,
        }
    }

    fn preferred_audio_stream(&self, camera_id: &str) -> &'static str {
        let policies = self
            .policies
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match policies.get(camera_id).map(|policy| policy.mode) {
            Some(CameraRecordingMode::Main | CameraRecordingMode::Both) => "main",
            Some(CameraRecordingMode::EventBoost)
                if policies
                    .get(camera_id)
                    .is_some_and(|policy| policy.event_main_state == EventMainState::Recording) =>
            {
                "main"
            }
            _ => "sub",
        }
    }
}

#[derive(Clone)]
pub struct StorageHandle {
    tx: mpsc::Sender<Command>,
    admission: RecordingAdmission,
}

impl StorageHandle {
    pub fn ingest(&self, camera_id: &str, frame: RecordingFrame) {
        self.ingest_stream(RecordingStreamIdentity::legacy(camera_id), frame);
    }

    pub fn ingest_stream(&self, identity: RecordingStreamIdentity, frame: RecordingFrame) {
        self.admission
            .ingest_at(&self.tx, identity, frame, Instant::now());
    }

    pub fn configure_camera_recording(
        &self,
        camera_id: &str,
        mode: CameraRecordingMode,
        event_duration: Duration,
    ) {
        self.admission.configure(camera_id, mode, event_duration);
    }

    pub fn note_camera_event(&self, camera_id: &str) {
        self.admission.note_event_at(camera_id, Instant::now());
    }

    pub fn preferred_audio_stream(&self, camera_id: &str) -> &'static str {
        self.admission.preferred_audio_stream(camera_id)
    }
}

pub struct StorageEngine {
    tx: mpsc::Sender<Command>,
    demand: RecordingDemand,
    admission: RecordingAdmission,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StorageEngine {
    pub fn start(config: StorageConfig) -> Self {
        Self::start_inner(config, None)
    }

    pub fn start_with_catalog(config: StorageConfig, catalog: RecordingCatalogHandle) -> Self {
        Self::start_inner(config, Some(catalog))
    }

    fn start_inner(config: StorageConfig, catalog: Option<RecordingCatalogHandle>) -> Self {
        let mut removed_active = cleanup_stale_active_files(&config.medium_term_path);
        if config.medium_term_path != config.long_term_path {
            removed_active.extend(cleanup_stale_active_files(&config.long_term_path));
        }
        if let Some(catalog) = &catalog
            && let Err(error) = catalog.delete_recordings_by_path(&removed_active)
        {
            tracing::warn!(%error, "unable to remove stale active recording catalog rows");
        }

        tracing::info!(
            medium_term_path = %config.medium_term_path.display(),
            long_term_path = %config.long_term_path.display(),
            recording_catalog_path = %config.recording_catalog_path.display(),
            event_thumbnail_path = %config.event_thumbnail_path.display(),
            event_thumbnail_max_mb = config.event_thumbnail_max_bytes / MEBIBYTE_BYTES,
            short_term_secs = config.short_term_duration.as_secs(),
            medium_term_secs = config.medium_term_duration.as_secs(),
            flush_interval_secs = config.flush_interval.as_secs(),
            write_buffer_bytes = config.write_buffer_bytes,
            direct_write = config.is_direct_write(),
            long_term_max_gb = config.long_term_max_bytes / GIBIBYTE_BYTES,
            "storage engine initialized",
        );

        let demand = RecordingDemand::new(DEMAND_INACTIVITY_GRACE);
        let admission = RecordingAdmission::default();
        let worker_demand = demand.clone();
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("storage-writer".into())
            .spawn(move || {
                let mut worker = WriterWorker::new(config, worker_demand, catalog);
                worker.run(rx);
            })
            .expect("failed to spawn storage writer thread");

        Self {
            tx,
            demand,
            admission,
            thread: Some(thread),
        }
    }

    pub fn handle(&self) -> StorageHandle {
        StorageHandle {
            tx: self.tx.clone(),
            admission: self.admission.clone(),
        }
    }

    pub fn demand(&self) -> RecordingDemand {
        self.demand.clone()
    }

    pub fn ingest(&self, camera_id: &str, frame: RecordingFrame) {
        self.ingest_stream(RecordingStreamIdentity::legacy(camera_id), frame);
    }

    pub fn ingest_stream(&self, identity: RecordingStreamIdentity, frame: RecordingFrame) {
        let _ = self.tx.send(Command::Ingest { identity, frame });
    }

    pub fn flush_all(&self) {
        let _ = self.tx.send(Command::FlushAll);
    }

    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        tracing::debug!("sending shutdown command to storage writer thread");
        let _ = self.tx.send(Command::Shutdown);
        if let Some(handle) = self.thread.take() {
            tracing::debug!("waiting for storage writer thread to finish");
            if let Err(panic) = handle.join() {
                tracing::error!("storage writer thread panicked: {:?}", panic);
            }
            tracing::debug!("storage writer thread finished");
        }
    }
}

impl Drop for StorageEngine {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

struct WriterWorker {
    config: StorageConfig,
    demand: RecordingDemand,
    catalog: Option<RecordingCatalogHandle>,
    pipelines: HashMap<String, CameraPipeline>,
    long_term: LongTermStore,
}

impl WriterWorker {
    fn new(
        config: StorageConfig,
        demand: RecordingDemand,
        catalog: Option<RecordingCatalogHandle>,
    ) -> Self {
        let long_term = LongTermStore::new(config.long_term_path.clone());
        Self {
            config,
            demand,
            catalog,
            pipelines: HashMap::new(),
            long_term,
        }
    }

    fn run(&mut self, rx: mpsc::Receiver<Command>) {
        let reap_interval = Duration::from_secs(300);
        let mut last_reap = Instant::now();

        self.enforce_storage_limit();

        loop {
            let cmd = if self.config.long_term_max_bytes > 0 {
                match rx.recv_timeout(reap_interval) {
                    Ok(cmd) => Some(cmd),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match rx.recv() {
                    Ok(cmd) => Some(cmd),
                    Err(_) => break,
                }
            };

            if let Some(cmd) = cmd {
                match cmd {
                    Command::Ingest { identity, frame } => {
                        self.ingest(identity, frame);
                    }
                    Command::FlushAll => {
                        self.flush_all();
                    }
                    Command::Shutdown => {
                        tracing::debug!("storage writer received shutdown command");
                        self.shutdown_flush();
                        tracing::debug!("shutdown flush complete, finalizing segments");
                        self.finalize_all();
                        tracing::debug!("all segments finalized");
                        break;
                    }
                }
            }

            if self.config.long_term_max_bytes > 0 && last_reap.elapsed() >= reap_interval {
                last_reap = Instant::now();
                self.enforce_storage_limit();
            }
        }
    }

    fn enforce_storage_limit(&self) {
        if self.config.long_term_max_bytes == 0 {
            return;
        }
        let removed = self
            .long_term
            .enforce_limit_with_removed(self.config.long_term_max_bytes);
        if let Some(catalog) = &self.catalog
            && let Err(error) = catalog.delete_recordings_by_path(&removed)
        {
            tracing::warn!(%error, "unable to remove retained recording catalog rows");
        }
    }

    fn ingest(&mut self, identity: RecordingStreamIdentity, frame: RecordingFrame) {
        let storage_key = identity.storage_key.clone();
        self.pipeline_for(identity);

        if self.config.is_direct_write() {
            if let Err(e) = self.direct_write(&storage_key, frame) {
                tracing::error!(camera = storage_key, error = %e, "direct write failed");
            }
            return;
        }

        let active = self.demand.is_active(&storage_key);
        let pipeline = self.pipelines.get_mut(&storage_key).unwrap();
        pipeline.short_term.push(frame);
        let needs_flush = active || pipeline.last_flush.elapsed() >= self.config.flush_interval;

        if needs_flush && let Err(e) = self.flush_camera(&storage_key, active) {
            tracing::error!(camera = storage_key, error = %e, "flush to medium-term failed");
        }
    }

    fn direct_write(&mut self, camera_id: &str, frame: RecordingFrame) -> std::io::Result<()> {
        let pipeline = self.pipelines.get_mut(camera_id).unwrap();

        let needs_rotation = pipeline
            .medium_term
            .as_ref()
            .is_some_and(|w| w.elapsed() >= self.config.medium_term_duration);

        let mut rotated = None;
        if needs_rotation && frame.is_video_keyframe() {
            let old = pipeline.medium_term.take().unwrap();
            let recording_id = old.recording_id().to_owned();
            rotated = Some((old.finalize()?, recording_id));
        }

        if pipeline.medium_term.is_none() {
            if !frame.is_video_keyframe() {
                return Ok(());
            }
            let started_at = frame.received_at;
            let writer = create_medium_term_writer(
                &self.config,
                self.catalog.as_ref(),
                &pipeline.identity,
                started_at,
            )?;
            tracing::info!(
                camera = camera_id,
                path = %writer.active_path().display(),
                "new segment started (direct-write)",
            );
            pipeline.medium_term = Some(writer);
        }

        pipeline.medium_term.as_mut().unwrap().append_one(frame)?;

        if let Some((path, recording_id)) = rotated {
            self.move_to_long_term(camera_id, &path, &recording_id)?;
        }

        Ok(())
    }

    fn flush_camera(&mut self, camera_id: &str, publish_all: bool) -> std::io::Result<()> {
        let pipeline = self.pipelines.get_mut(camera_id).unwrap();
        pipeline.last_flush = Instant::now();

        let mut frames = if publish_all {
            pipeline.short_term.drain_all()
        } else {
            let cutoff = Instant::now() - self.config.short_term_duration;
            pipeline.short_term.drain_up_to_last_keyframe_before(cutoff)
        };
        if frames.is_empty() {
            return Ok(());
        }

        let needs_rotation = pipeline
            .medium_term
            .as_ref()
            .is_some_and(|w| w.elapsed() >= self.config.medium_term_duration);

        let mut rotated = None;

        if needs_rotation {
            if let Some(kf_idx) = frames.iter().position(|f| f.is_video_keyframe()) {
                if kf_idx > 0 {
                    let remainder = frames.split_off(kf_idx);
                    pipeline
                        .medium_term
                        .as_mut()
                        .unwrap()
                        .append_batch(frames)?;
                    frames = remainder;
                }
            } else {
                pipeline
                    .medium_term
                    .as_mut()
                    .unwrap()
                    .append_batch(frames)?;
                frames = Vec::new();
            }

            let old = pipeline.medium_term.take().unwrap();
            let recording_id = old.recording_id().to_owned();
            rotated = Some((old.finalize()?, recording_id));
        }

        if !frames.is_empty() && pipeline.medium_term.is_none() {
            if let Some(keyframe_index) = frames.iter().position(RecordingFrame::is_video_keyframe)
            {
                if keyframe_index > 0 {
                    frames = frames.split_off(keyframe_index);
                }
                let started_at = frames[0].received_at;
                let writer = create_medium_term_writer(
                    &self.config,
                    self.catalog.as_ref(),
                    &pipeline.identity,
                    started_at,
                )?;
                tracing::info!(
                    camera = camera_id,
                    path = %writer.active_path().display(),
                    "new medium-term segment started",
                );
                pipeline.medium_term = Some(writer);
            } else {
                frames.clear();
            }
        }

        if !frames.is_empty() {
            pipeline
                .medium_term
                .as_mut()
                .unwrap()
                .append_batch(frames)?;
        }

        if let Some((path, recording_id)) = rotated {
            self.move_to_long_term(camera_id, &path, &recording_id)?;
        }

        Ok(())
    }

    fn flush_all(&mut self) {
        let ids: Vec<String> = self.pipelines.keys().cloned().collect();
        for id in ids {
            if let Err(e) = self.flush_camera(&id, false) {
                tracing::error!(camera = %id, error = %e, "flush failed");
            }
        }
    }

    fn shutdown_flush(&mut self) {
        self.flush_all();

        for (id, pipeline) in &mut self.pipelines {
            let frames = pipeline.short_term.drain_all();
            if frames.is_empty() {
                continue;
            }

            let mut frames = frames;
            if pipeline.medium_term.is_none() {
                let Some(keyframe_index) =
                    frames.iter().position(RecordingFrame::is_video_keyframe)
                else {
                    continue;
                };
                if keyframe_index > 0 {
                    frames = frames.split_off(keyframe_index);
                }
                let started_at = frames[0].received_at;
                match create_medium_term_writer(
                    &self.config,
                    self.catalog.as_ref(),
                    &pipeline.identity,
                    started_at,
                ) {
                    Ok(writer) => pipeline.medium_term = Some(writer),
                    Err(e) => {
                        tracing::error!(camera = %id, error = %e, "failed to create segment on shutdown");
                        continue;
                    }
                }
            }

            if let Err(e) = pipeline.medium_term.as_mut().unwrap().append_batch(frames) {
                tracing::error!(camera = %id, error = %e, "failed to write remaining frames on shutdown");
            }
        }
    }

    fn finalize_all(&mut self) {
        let ids: Vec<String> = self.pipelines.keys().cloned().collect();
        for id in ids {
            if let Some(writer) = self
                .pipelines
                .get_mut(&id)
                .and_then(|p| p.medium_term.take())
            {
                let recording_id = writer.recording_id().to_owned();
                match writer.finalize() {
                    Ok(path) => {
                        if let Err(e) = self.move_to_long_term(&id, &path, &recording_id) {
                            tracing::error!(camera = %id, error = %e, "move to long-term failed");
                        }
                    }
                    Err(e) => {
                        tracing::error!(camera = %id, error = %e, "finalize failed");
                    }
                }
            }
        }
    }

    fn move_to_long_term(
        &self,
        camera_id: &str,
        path: &Path,
        recording_id: &str,
    ) -> std::io::Result<PathBuf> {
        let destination = if self.config.medium_term_path == self.config.long_term_path {
            tracing::info!(
                camera = camera_id,
                path = %path.display(),
                "segment finalized to long-term storage",
            );
            path.to_path_buf()
        } else {
            let rel = path
                .strip_prefix(&self.config.medium_term_path)
                .unwrap_or(path);
            let destination = self.config.long_term_path.join(rel);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }

            std::fs::rename(path, &destination).or_else(|_| {
                std::fs::copy(path, &destination)?;
                std::fs::remove_file(path)?;
                Ok::<(), std::io::Error>(())
            })?;

            tracing::info!(
                camera = camera_id,
                from = %path.display(),
                to = %destination.display(),
                "segment moved to long-term storage",
            );
            destination
        };
        if let Some(catalog) = &self.catalog {
            catalog
                .update_recording_path(recording_id, &destination, true)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
        Ok(destination)
    }

    fn pipeline_for(&mut self, identity: RecordingStreamIdentity) -> &mut CameraPipeline {
        let buffer_window = self.config.short_term_duration + self.config.flush_interval;
        self.pipelines
            .entry(identity.storage_key.clone())
            .or_insert_with(|| CameraPipeline {
                identity,
                short_term: ShortTermBuffer::new(buffer_window),
                medium_term: None,
                last_flush: Instant::now(),
            })
    }
}

fn create_medium_term_writer(
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    identity: &RecordingStreamIdentity,
    started_at: Instant,
) -> std::io::Result<MediumTermWriter> {
    catalog.map_or_else(
        || {
            MediumTermWriter::create(
                &config.medium_term_path,
                &identity.storage_key,
                started_at,
                config.write_buffer_bytes,
            )
        },
        |catalog| {
            MediumTermWriter::create_with_catalog_identity(
                &config.medium_term_path,
                identity.clone(),
                started_at,
                config.write_buffer_bytes,
                catalog.clone(),
            )
        },
    )
}

pub struct ShortTermStats {
    pub chunks: usize,
    pub bytes: usize,
    pub duration: Duration,
}

fn cleanup_stale_active_files(root: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let walker = match std::fs::read_dir(root) {
        Ok(w) => w,
        Err(_) => return removed,
    };
    fn walk(dir: std::fs::ReadDir, removed: &mut Vec<PathBuf>) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(sub) = std::fs::read_dir(&path) {
                    walk(sub, removed);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("active") {
                tracing::warn!(path = %path.display(), "removing stale active segment from previous run");
                if std::fs::remove_file(&path).is_ok() {
                    removed.push(path);
                }
            }
        }
    }
    walk(walker, &mut removed);
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        AudioCodec, AudioFrame, CatalogRecording, MediaFrame, RecordingCatalog, VideoCodec,
        VideoFrame,
    };
    use bytes::Bytes;
    use std::fs::File;

    fn storage_config(name: &str) -> StorageConfig {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-output")
            .join(name);
        StorageConfig {
            medium_term_path: root.clone(),
            long_term_path: root.clone(),
            recording_catalog_path: root.join("recordings.db"),
            event_thumbnail_path: root.join(".event-thumbnails"),
            event_thumbnail_max_bytes: 1_024 * 1_048_576,
            short_term_duration: Duration::from_secs(60),
            medium_term_duration: Duration::from_secs(1_800),
            flush_interval: Duration::from_secs(60),
            write_buffer_bytes: 8 * 1024,
            long_term_max_bytes: 0,
        }
    }

    #[test]
    fn storage_config_derives_metadata_paths_from_long_term_storage() {
        let storage = StorageConfig::from_toml(&StorageToml {
            long_term_path: Some("/archive/keeppeek".to_owned()),
            event_thumbnail_max_mb: 512,
            ..StorageToml::default()
        });

        assert_eq!(
            storage.recording_catalog_path,
            PathBuf::from("/archive/keeppeek/recordings.db")
        );
        assert_eq!(
            storage.event_thumbnail_path,
            PathBuf::from("/archive/keeppeek/.event-thumbnails")
        );
        assert_eq!(storage.event_thumbnail_max_bytes, 512 * MEBIBYTE_BYTES);
        assert_eq!(storage.long_term_max_bytes, 1_024 * GIBIBYTE_BYTES);
    }

    #[test]
    fn recording_admission_enforces_modes_and_keyframe_aligned_event_boost() {
        let admission = RecordingAdmission::default();
        let now = Instant::now();

        admission.configure("off", CameraRecordingMode::Off, Duration::from_secs(60));
        admission.configure("sub", CameraRecordingMode::Sub, Duration::from_secs(60));
        admission.configure("main", CameraRecordingMode::Main, Duration::from_secs(60));
        admission.configure("both", CameraRecordingMode::Both, Duration::from_secs(60));
        admission.configure(
            "event",
            CameraRecordingMode::EventBoost,
            Duration::from_secs(60),
        );

        assert!(matches!(
            admission.decide_at("off", "sub", true, false, now),
            AdmissionDecision::Ignore
        ));
        assert!(matches!(
            admission.decide_at("off", "main", true, true, now),
            AdmissionDecision::Ignore
        ));
        assert!(matches!(
            admission.decide_at("sub", "sub", true, false, now),
            AdmissionDecision::Record
        ));
        assert!(matches!(
            admission.decide_at("sub", "main", true, true, now),
            AdmissionDecision::Ignore
        ));
        assert!(matches!(
            admission.decide_at("main", "main", true, false, now),
            AdmissionDecision::Record
        ));
        assert!(matches!(
            admission.decide_at("main", "sub", true, false, now),
            AdmissionDecision::Ignore
        ));
        assert!(matches!(
            admission.decide_at("both", "main", true, false, now),
            AdmissionDecision::Record
        ));
        assert!(matches!(
            admission.decide_at("both", "sub", true, false, now),
            AdmissionDecision::Record
        ));
        assert!(matches!(
            admission.decide_at("event", "sub", true, false, now),
            AdmissionDecision::RecordAs("sub")
        ));
        assert!(matches!(
            admission.decide_at("event", "main", true, true, now),
            AdmissionDecision::Ignore
        ));

        admission.note_event_at("event", now);
        assert!(matches!(
            admission.decide_at("event", "main", true, false, now + Duration::from_secs(1)),
            AdmissionDecision::Ignore
        ));
        assert!(matches!(
            admission.decide_at("event", "sub", true, false, now + Duration::from_secs(1)),
            AdmissionDecision::RecordAs("sub")
        ));
        assert!(matches!(
            admission.decide_at("event", "main", true, true, now + Duration::from_secs(2)),
            AdmissionDecision::RecordAs("sub")
        ));
        assert_eq!(admission.preferred_audio_stream("event"), "main");
        assert!(matches!(
            admission.decide_at("event", "sub", true, false, now + Duration::from_secs(3)),
            AdmissionDecision::Ignore
        ));
        assert!(matches!(
            admission.decide_at("event", "main", false, false, now + Duration::from_secs(3)),
            AdmissionDecision::RecordAs("sub")
        ));
        admission.note_event_at("event", now + Duration::from_secs(30));
        assert!(matches!(
            admission.decide_at("event", "main", true, false, now + Duration::from_secs(89)),
            AdmissionDecision::RecordAs("sub")
        ));
        assert!(matches!(
            admission.decide_at("event", "main", true, false, now + Duration::from_secs(90)),
            AdmissionDecision::RecordAs("sub")
        ));
        admission.note_event_at("event", now + Duration::from_secs(90));
        assert!(matches!(
            admission.decide_at("event", "sub", true, true, now + Duration::from_secs(91)),
            AdmissionDecision::Ignore
        ));
        assert!(matches!(
            admission.decide_at("event", "main", true, false, now + Duration::from_secs(150)),
            AdmissionDecision::RecordAs("sub")
        ));
        assert!(matches!(
            admission.decide_at("event", "sub", true, false, now + Duration::from_secs(151)),
            AdmissionDecision::Ignore
        ));
        assert!(matches!(
            admission.decide_at("event", "sub", true, true, now + Duration::from_secs(152)),
            AdmissionDecision::RecordAs("sub")
        ));
        assert_eq!(admission.preferred_audio_stream("event"), "sub");
        assert!(matches!(
            admission.decide_at("event", "main", true, false, now + Duration::from_secs(153)),
            AdmissionDecision::Ignore
        ));

        admission.configure(
            "expired",
            CameraRecordingMode::EventBoost,
            Duration::from_secs(60),
        );
        admission.note_event_at("expired", now);
        assert!(matches!(
            admission.decide_at("expired", "main", true, true, now + Duration::from_secs(60)),
            AdmissionDecision::Ignore
        ));
        assert!(matches!(
            admission.decide_at("expired", "sub", true, false, now + Duration::from_secs(61)),
            AdmissionDecision::RecordAs("sub")
        ));
    }

    #[test]
    fn admission_and_enqueue_are_atomic_across_source_threads() {
        let admission = RecordingAdmission::default();
        let now = Instant::now();
        admission.configure(
            "camera",
            CameraRecordingMode::EventBoost,
            Duration::from_secs(60),
        );
        admission.note_event_at("camera", now);

        let (command_tx, command_rx) = mpsc::channel();
        let (sub_paused_tx, sub_paused_rx) = mpsc::sync_channel(0);
        let (release_sub_tx, release_sub_rx) = mpsc::sync_channel(0);
        let sub_admission = admission.clone();
        let sub_command_tx = command_tx.clone();
        let sub_thread = std::thread::spawn(move || {
            sub_admission.ingest_with_hook_at(
                &sub_command_tx,
                RecordingStreamIdentity::new("camera", "sub", "camera"),
                inter_frame(),
                now + Duration::from_secs(1),
                || {
                    sub_paused_tx.send(()).unwrap();
                    release_sub_rx.recv().unwrap();
                },
            );
        });
        sub_paused_rx.recv().unwrap();

        let (main_started_tx, main_started_rx) = mpsc::sync_channel(0);
        let (main_admitted_tx, main_admitted_rx) = mpsc::sync_channel(1);
        let main_admission = admission;
        let main_command_tx = command_tx;
        let main_thread = std::thread::spawn(move || {
            main_started_tx.send(()).unwrap();
            main_admission.ingest_with_hook_at(
                &main_command_tx,
                RecordingStreamIdentity::new("camera", "main", "camera"),
                key_frame(now + Duration::from_secs(2)),
                now + Duration::from_secs(2),
                || main_admitted_tx.send(()).unwrap(),
            );
        });
        main_started_rx.recv().unwrap();
        let main_overtook_sub = main_admitted_rx
            .recv_timeout(Duration::from_millis(100))
            .is_ok();

        release_sub_tx.send(()).unwrap();
        sub_thread.join().unwrap();
        main_thread.join().unwrap();
        assert!(
            !main_overtook_sub,
            "main admission overtook a paused sub enqueue"
        );

        let Command::Ingest {
            identity: first_identity,
            frame: first_frame,
        } = command_rx.recv().unwrap()
        else {
            panic!("first command must ingest the admitted sub frame");
        };
        let Command::Ingest {
            identity: second_identity,
            frame: second_frame,
        } = command_rx.recv().unwrap()
        else {
            panic!("second command must ingest the switching main keyframe");
        };
        assert_eq!(first_identity.storage_key, "camera/sub");
        assert!(!first_frame.is_video_keyframe());
        assert_eq!(second_identity.storage_key, "camera/sub");
        assert!(second_frame.is_video_keyframe());
    }

    fn inter_frame() -> RecordingFrame {
        RecordingFrame {
            received_at: Instant::now(),
            timestamp: None,
            frame: MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H264,
                is_keyframe: false,
                width: 320,
                height: 240,
                data: vec![0; 16].into(),
            }),
        }
    }

    fn key_frame(received_at: Instant) -> RecordingFrame {
        RecordingFrame {
            received_at,
            timestamp: None,
            frame: MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H264,
                is_keyframe: true,
                width: 320,
                height: 240,
                data: vec![
                    0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68,
                    0xce, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
                ]
                .into(),
            }),
        }
    }

    fn h265_key_frame(received_at: Instant) -> RecordingFrame {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata/cc-4k-640x360-h265.mp4");
        let mut input = mp4::read_mp4(File::open(fixture).unwrap()).unwrap();
        let (&track_id, track) = input
            .tracks()
            .iter()
            .find(|(_, track)| track.media_type().ok() == Some(mp4::MediaType::H265))
            .unwrap();
        let decoder = track.video_decoder_config().unwrap().unwrap();
        let mp4::MediaConfig::HevcConfig(config) = track.media_config_for_description(1).unwrap()
        else {
            panic!("H.265 fixture must expose HEVC configuration");
        };
        let sample = (1..=track.sample_count())
            .find_map(|sample_id| {
                let sample = input.read_sample(track_id, sample_id).unwrap().unwrap();
                sample.is_sync.then_some(sample.bytes)
            })
            .unwrap();
        let mut data = Vec::new();
        for parameter_set in [&config.vps, &config.sps, &config.pps] {
            data.extend_from_slice(&u32::try_from(parameter_set.len()).unwrap().to_be_bytes());
            data.extend_from_slice(parameter_set);
        }
        data.extend_from_slice(&sample);
        RecordingFrame {
            received_at,
            timestamp: None,
            frame: MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H265,
                is_keyframe: true,
                width: u32::from(decoder.width),
                height: u32::from(decoder.height),
                data: Bytes::from(data),
            }),
        }
    }

    fn audio_frame(received_at: Instant) -> RecordingFrame {
        RecordingFrame {
            received_at,
            timestamp: None,
            frame: MediaFrame::Audio(AudioFrame {
                codec: AudioCodec::Aac,
                sample_rate: 48_000,
                duration: Duration::from_millis(20),
                data: vec![0xff, 0xf1, 0x4c, 0x40, 0, 0, 0, 0xaa],
            }),
        }
    }

    #[test]
    fn event_boost_writes_sub_and_main_gops_to_one_sub_recording() {
        let mut config = storage_config("event-boost-single-recording");
        let root = config.long_term_path.clone();
        let _ = std::fs::remove_dir_all(&root);
        config.short_term_duration = Duration::ZERO;
        config.flush_interval = Duration::ZERO;
        let engine = StorageEngine::start(config);
        let storage = engine.handle();
        storage.configure_camera_recording(
            "camera",
            CameraRecordingMode::EventBoost,
            Duration::from_secs(60),
        );
        let started_at = Instant::now();
        for offset in [0, 40] {
            storage.ingest_stream(
                RecordingStreamIdentity::new("camera", "sub", "camera"),
                key_frame(started_at + Duration::from_millis(offset)),
            );
        }
        storage.note_camera_event("camera");
        for offset in [80, 120] {
            storage.ingest_stream(
                RecordingStreamIdentity::new("camera", "main", "camera"),
                key_frame(started_at + Duration::from_millis(offset)),
            );
        }
        engine.shutdown();

        let store = LongTermStore::new(root.clone());
        let sub_recordings = store.finalized_segments("camera/sub").unwrap();
        assert_eq!(sub_recordings.len(), 1);
        assert!(store.finalized_segments("camera/main").unwrap().is_empty());
        let reader = mp4::read_mp4(std::fs::File::open(&sub_recordings[0]).unwrap()).unwrap();
        let video = reader
            .tracks()
            .values()
            .find(|track| matches!(track.track_type(), Ok(mp4::TrackType::Video)))
            .unwrap();
        assert_eq!(video.sample_count(), 4);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn event_boost_round_trips_h264_h265_h264_with_audio_and_catalog() {
        let mut config = storage_config("event-boost-full-handoff");
        let root = config.long_term_path.clone();
        let _ = std::fs::remove_dir_all(&root);
        config.short_term_duration = Duration::ZERO;
        config.flush_interval = Duration::ZERO;
        let catalog = RecordingCatalog::open(&config.recording_catalog_path).unwrap();
        let catalog_handle = catalog.handle();
        let engine = StorageEngine::start_with_catalog(config, catalog_handle.clone());
        let storage = engine.handle();
        storage.configure_camera_recording(
            "camera",
            CameraRecordingMode::EventBoost,
            Duration::from_millis(60),
        );
        let now = Instant::now();
        let ingest = |stream: &str, frame: RecordingFrame, at: Instant| {
            storage.admission.ingest_at(
                &storage.tx,
                RecordingStreamIdentity::new("camera", stream, "camera"),
                frame,
                at,
            );
        };

        ingest("sub", key_frame(now), now);
        ingest(
            "sub",
            audio_frame(now + Duration::from_millis(10)),
            now + Duration::from_millis(10),
        );
        ingest(
            "sub",
            key_frame(now + Duration::from_millis(40)),
            now + Duration::from_millis(40),
        );
        storage
            .admission
            .note_event_at("camera", now + Duration::from_millis(50));
        ingest(
            "main",
            h265_key_frame(now + Duration::from_millis(80)),
            now + Duration::from_millis(80),
        );
        ingest(
            "main",
            audio_frame(now + Duration::from_millis(90)),
            now + Duration::from_millis(90),
        );
        storage
            .admission
            .note_event_at("camera", now + Duration::from_millis(100));
        ingest(
            "main",
            h265_key_frame(now + Duration::from_millis(140)),
            now + Duration::from_millis(140),
        );
        ingest(
            "sub",
            key_frame(now + Duration::from_millis(150)),
            now + Duration::from_millis(150),
        );
        ingest(
            "main",
            h265_key_frame(now + Duration::from_millis(170)),
            now + Duration::from_millis(170),
        );
        ingest(
            "sub",
            key_frame(now + Duration::from_millis(180)),
            now + Duration::from_millis(180),
        );
        ingest(
            "sub",
            audio_frame(now + Duration::from_millis(190)),
            now + Duration::from_millis(190),
        );
        ingest(
            "sub",
            key_frame(now + Duration::from_millis(220)),
            now + Duration::from_millis(220),
        );
        engine.shutdown();

        let sub_fragments = catalog_handle
            .media_fragments_in_range("camera/sub", i64::MIN + 1, i64::MAX)
            .unwrap();
        assert_eq!(sub_fragments.len(), 7);
        assert!(
            catalog_handle
                .media_fragments_in_range("camera/main", i64::MIN + 1, i64::MAX)
                .unwrap()
                .is_empty()
        );
        let path = PathBuf::from(&sub_fragments[0].path);
        assert!(
            sub_fragments
                .iter()
                .all(|fragment| fragment.path == sub_fragments[0].path)
        );
        let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
        let (&video_track_id, video) = reader
            .tracks()
            .iter()
            .find(|(_, track)| track.track_type().ok() == Some(mp4::TrackType::Video))
            .unwrap();
        let (&audio_track_id, audio) = reader
            .tracks()
            .iter()
            .find(|(_, track)| track.track_type().ok() == Some(mp4::TrackType::Audio))
            .unwrap();
        assert_eq!(video.sample_description_count(), 2);
        assert_eq!(video.sample_count(), 7);
        assert_eq!(
            (1..=video.sample_count())
                .map(|sample_id| video.sample_description_index(sample_id).unwrap())
                .collect::<Vec<_>>(),
            vec![1, 1, 2, 2, 2, 1, 1]
        );
        assert_eq!(audio.sample_count(), 3);
        let audio_starts = (1..=audio.sample_count())
            .map(|sample_id| {
                reader
                    .read_sample(audio_track_id, sample_id)
                    .unwrap()
                    .unwrap()
                    .start_time
            })
            .collect::<Vec<_>>();
        assert!(audio_starts.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(reader.sample_count(video_track_id).unwrap(), 7);

        drop(catalog_handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_demand_drains_idle_frames_immediately() {
        const STREAM: &str = "front-door/main";
        let demand = RecordingDemand::new(Duration::ZERO);
        let mut worker = WriterWorker::new(storage_config("adaptive-demand"), demand.clone(), None);

        worker.ingest(RecordingStreamIdentity::legacy(STREAM), inter_frame());
        assert_eq!(worker.pipelines[STREAM].short_term.len(), 1);

        let guard = demand.acquire(STREAM);
        worker.ingest(RecordingStreamIdentity::legacy(STREAM), inter_frame());
        assert!(worker.pipelines[STREAM].short_term.is_empty());

        drop(guard);
        worker.ingest(RecordingStreamIdentity::legacy(STREAM), inter_frame());
        assert_eq!(worker.pipelines[STREAM].short_term.len(), 1);
    }

    #[test]
    fn direct_write_waits_for_a_server_timestamped_keyframe() {
        const STREAM: &str = "front-door/main";
        let demand = RecordingDemand::new(Duration::ZERO);
        let mut config = storage_config("direct-write-keyframe-start");
        config.short_term_duration = Duration::ZERO;
        config.flush_interval = Duration::ZERO;
        let mut worker = WriterWorker::new(config, demand, None);

        worker.ingest(RecordingStreamIdentity::legacy(STREAM), inter_frame());

        assert!(worker.pipelines[STREAM].medium_term.is_none());
    }

    #[test]
    fn startup_removes_stale_active_file_and_its_catalog_row() {
        let config = storage_config("stale-active-recovery");
        let root = config.long_term_path.clone();
        let _ = std::fs::remove_dir_all(&root);
        let active_path = root
            .join("camera")
            .join("sub")
            .join("2026-08-24")
            .join("15")
            .join("recording.mp4.active");
        std::fs::create_dir_all(active_path.parent().unwrap()).unwrap();
        std::fs::write(&active_path, b"interrupted recording").unwrap();
        let catalog = RecordingCatalog::open(&config.recording_catalog_path).unwrap();
        let catalog_handle = catalog.handle();
        catalog_handle
            .upsert_recording(CatalogRecording {
                id: "interrupted".to_owned(),
                stream_id: "camera/sub".to_owned(),
                source_id: Some("camera".to_owned()),
                logical_stream_id: Some("sub".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: None,
                path: active_path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 0,
                finalized: false,
            })
            .unwrap();

        let engine = StorageEngine::start_with_catalog(config, catalog_handle.clone());
        engine.shutdown();

        assert!(!active_path.exists());
        assert_eq!(catalog_handle.stats().unwrap().recording_files, 0);
        drop(catalog_handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
}
