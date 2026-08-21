use crate::{
    config::StorageToml,
    storage::{
        catalog::RecordingCatalogHandle, demand::RecordingDemand, long_term::LongTermStore,
        medium_term::MediumTermWriter, segment::RecordingFrame, short_term::ShortTermBuffer,
    },
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc,
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
        camera_id: String,
        frame: RecordingFrame,
    },
    FlushAll,
    Shutdown,
}

struct CameraPipeline {
    short_term: ShortTermBuffer,
    medium_term: Option<MediumTermWriter>,
    last_flush: Instant,
}

#[derive(Clone)]
pub struct StorageHandle {
    tx: mpsc::Sender<Command>,
}

impl StorageHandle {
    pub fn ingest(&self, camera_id: &str, frame: RecordingFrame) {
        let _ = self.tx.send(Command::Ingest {
            camera_id: camera_id.to_owned(),
            frame,
        });
    }
}

pub struct StorageEngine {
    tx: mpsc::Sender<Command>,
    demand: RecordingDemand,
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
        cleanup_stale_active_files(&config.medium_term_path);
        if config.medium_term_path != config.long_term_path {
            cleanup_stale_active_files(&config.long_term_path);
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
            thread: Some(thread),
        }
    }

    pub fn handle(&self) -> StorageHandle {
        StorageHandle {
            tx: self.tx.clone(),
        }
    }

    pub fn demand(&self) -> RecordingDemand {
        self.demand.clone()
    }

    pub fn ingest(&self, camera_id: &str, frame: RecordingFrame) {
        let _ = self.tx.send(Command::Ingest {
            camera_id: camera_id.to_owned(),
            frame,
        });
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
                    Command::Ingest { camera_id, frame } => {
                        self.ingest(&camera_id, frame);
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
                self.long_term
                    .enforce_limit(self.config.long_term_max_bytes);
            }
        }
    }

    fn ingest(&mut self, camera_id: &str, frame: RecordingFrame) {
        self.pipeline_for(camera_id);

        if self.config.is_direct_write() {
            if let Err(e) = self.direct_write(camera_id, frame) {
                tracing::error!(camera = camera_id, error = %e, "direct write failed");
            }
            return;
        }

        let active = self.demand.is_active(camera_id);
        let pipeline = self.pipelines.get_mut(camera_id).unwrap();
        pipeline.short_term.push(frame);
        let needs_flush = active || pipeline.last_flush.elapsed() >= self.config.flush_interval;

        if needs_flush && let Err(e) = self.flush_camera(camera_id, active) {
            tracing::error!(camera = camera_id, error = %e, "flush to medium-term failed");
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
                camera_id,
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
                    camera_id,
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
                match create_medium_term_writer(&self.config, self.catalog.as_ref(), id, started_at)
                {
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

    fn pipeline_for(&mut self, camera_id: &str) -> &mut CameraPipeline {
        let buffer_window = self.config.short_term_duration + self.config.flush_interval;
        self.pipelines
            .entry(camera_id.to_owned())
            .or_insert_with(|| CameraPipeline {
                short_term: ShortTermBuffer::new(buffer_window),
                medium_term: None,
                last_flush: Instant::now(),
            })
    }
}

fn create_medium_term_writer(
    config: &StorageConfig,
    catalog: Option<&RecordingCatalogHandle>,
    camera_id: &str,
    started_at: Instant,
) -> std::io::Result<MediumTermWriter> {
    catalog.map_or_else(
        || {
            MediumTermWriter::create(
                &config.medium_term_path,
                camera_id,
                started_at,
                config.write_buffer_bytes,
            )
        },
        |catalog| {
            MediumTermWriter::create_with_catalog(
                &config.medium_term_path,
                camera_id,
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

fn cleanup_stale_active_files(root: &Path) {
    let walker = match std::fs::read_dir(root) {
        Ok(w) => w,
        Err(_) => return,
    };
    fn walk(dir: std::fs::ReadDir) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(sub) = std::fs::read_dir(&path) {
                    walk(sub);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("active") {
                tracing::warn!(path = %path.display(), "removing stale active segment from previous run");
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    walk(walker);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{MediaFrame, VideoCodec, VideoFrame};

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

    #[test]
    fn active_demand_drains_idle_frames_immediately() {
        const STREAM: &str = "front-door/main";
        let demand = RecordingDemand::new(Duration::ZERO);
        let mut worker = WriterWorker::new(storage_config("adaptive-demand"), demand.clone(), None);

        worker.ingest(STREAM, inter_frame());
        assert_eq!(worker.pipelines[STREAM].short_term.len(), 1);

        let guard = demand.acquire(STREAM);
        worker.ingest(STREAM, inter_frame());
        assert!(worker.pipelines[STREAM].short_term.is_empty());

        drop(guard);
        worker.ingest(STREAM, inter_frame());
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

        worker.ingest(STREAM, inter_frame());

        assert!(worker.pipelines[STREAM].medium_term.is_none());
    }
}
