use super::*;
use crate::storage::event_recording::PreRecordReason;
#[cfg(test)]
mod tests;
use crate::{cameras::EventRecordingStream, privacy::PrivacyGate};
use std::sync::atomic::AtomicU64;

pub(super) struct EventAdmission {
    settings: EventSettings,
    generations: Arc<EventGenerations>,
    queued_frames: Arc<AtomicUsize>,
}

#[derive(Default)]
struct EventGenerations {
    source: AtomicU64,
    main: AtomicU64,
}

#[derive(Clone)]
pub(super) struct EventFence {
    generations: Arc<EventGenerations>,
    observed: u64,
    main_observed: u64,
    privacy: Option<(Arc<PrivacyGate>, u64)>,
}

impl EventFence {
    fn main_valid(&self) -> bool {
        self.generations.main.load(Ordering::Acquire) == self.main_observed
    }

    fn valid(&self) -> bool {
        self.generations.source.load(Ordering::Acquire) == self.observed
            && self
                .privacy
                .as_ref()
                .is_none_or(|(gate, epoch)| gate.allows(*epoch))
    }
}

pub(super) struct OptionalMainSlot(Arc<AtomicUsize>);

impl OptionalMainSlot {
    fn reserve(tx: &StorageCommandSender) -> Result<Self, ()> {
        tx.optional_main_slots
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |slots| {
                slots
                    .checked_add(1)
                    .filter(|total| *total <= tx.command_capacity / 2)
            })
            .map_err(|_| ())?;
        Ok(Self(Arc::clone(&tx.optional_main_slots)))
    }
}

impl Drop for OptionalMainSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn enqueue_event_input(
    tx: &StorageCommandSender,
    entry: &EventAdmission,
    identity: RecordingStreamIdentity,
    frame: RecordingFrame,
    fence: EventFence,
    optional_main: bool,
) -> Result<(), String> {
    let main_slot = if optional_main {
        match OptionalMainSlot::reserve(tx) {
            Ok(slot) => Some(slot),
            Err(()) => return Err(identity.storage_key),
        }
    } else {
        None
    };
    let max_bytes = if optional_main {
        tx.media_bytes_capacity / 2
    } else {
        tx.media_bytes_capacity
    };
    let input = match QueuedEventFrame::try_new(
        frame,
        Arc::clone(&tx.queued_media_bytes),
        Arc::clone(&entry.queued_frames),
        max_bytes,
        if optional_main { 512 } else { 1024 },
    ) {
        Ok(input) => input,
        Err(_) => return Err(identity.storage_key),
    };
    tx.tx
        .try_send(Command::EventInput {
            identity,
            input: Box::new(input),
            fence,
            main_slot,
        })
        .map_err(|error| {
            let (mpsc::TrySendError::Full(command) | mpsc::TrySendError::Disconnected(command)) =
                error;
            let Command::EventInput { identity, .. } = command else {
                unreachable!("only event input was sent")
            };
            identity.storage_key
        })
}

pub(super) struct EventSourceState {
    fence: EventFence,
    privacy_epoch: u64,
    privacy_active: bool,
    paused: bool,
}

fn enabled(settings: EventSettings) -> bool {
    settings.mode == CameraRecordingMode::EventOnly
        || (settings.mode == CameraRecordingMode::EventBoost && !settings.pre.is_zero())
}

const fn stream_name(stream: EventRecordingStream) -> &'static str {
    match stream {
        EventRecordingStream::Main => "main",
        EventRecordingStream::Sub => "sub",
    }
}

fn settings_valid(source: &str, settings: EventSettings) -> bool {
    !source.is_empty()
        && source.len() <= 256
        && settings.pre <= Duration::from_secs(30)
        && (!enabled(settings)
            || (!settings.post.is_zero() && settings.post <= Duration::from_secs(3600)))
}

impl RecordingAdmission {
    fn event_fence(&self, source: &str, entry: &EventAdmission) -> EventFence {
        let privacy = self
            .privacy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|registry| registry.gate(source))
            .map(|gate| {
                let epoch = gate.epoch();
                (gate, epoch)
            });
        EventFence {
            observed: entry.generations.source.load(Ordering::Acquire),
            generations: Arc::clone(&entry.generations),
            main_observed: entry.generations.main.load(Ordering::Acquire),
            privacy,
        }
    }

    pub(super) fn has_event_recording(&self, source: &str) -> bool {
        self.events_enabled.load(Ordering::Acquire)
            && self
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(source)
    }

    pub(super) fn ingest_event(
        &self,
        tx: &StorageCommandSender,
        entry: &EventAdmission,
        identity: RecordingStreamIdentity,
        frame: RecordingFrame,
    ) {
        if entry.settings.mode == CameraRecordingMode::EventOnly
            && identity.stream_id != stream_name(entry.settings.stream)
        {
            return;
        }
        if !matches!(identity.stream_id.as_str(), "main" | "sub") {
            return;
        }
        let fence = self.event_fence(&identity.source_id, entry);
        if !fence.valid() {
            return;
        }
        let optional_main =
            entry.settings.mode == CameraRecordingMode::EventBoost && identity.stream_id == "main";
        if let Err(storage_key) =
            enqueue_event_input(tx, entry, identity, frame, fence, optional_main)
        {
            let generation = if optional_main {
                &entry.generations.main
            } else {
                &entry.generations.source
            };
            generation.fetch_add(1, Ordering::AcqRel);
            self.health.note_failure(
                &storage_key,
                "event recording queue is full or disconnected",
            );
            tracing::warn!(
                optional_main,
                "event recording dropped input and invalidated affected history"
            );
        }
    }

    pub(super) fn note_event(&self, tx: &StorageCommandSender, source: &str, at: Instant) {
        if !self.has_event_recording(source) {
            self.note_event_at(source, at);
            return;
        }
        let _policies = self
            .policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let events = self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = events.get(source) else {
            return;
        };
        let fence = self.event_fence(source, entry);
        if !fence.valid() {
            return;
        }
        if tx
            .tx
            .try_send(Command::RecordingEvent {
                source: source.to_owned(),
                at,
                fence,
            })
            .is_err()
        {
            self.health.note_failure(
                source,
                "event recording trigger queue is full or disconnected",
            );
        }
    }
}

impl StorageHandle {
    pub fn configure_camera_event_recording(
        &self,
        source: &str,
        mode: CameraRecordingMode,
        stream: EventRecordingStream,
        pre: Duration,
        post: Duration,
    ) {
        self.configure_event_settings(
            source,
            EventSettings {
                mode,
                stream,
                pre,
                post,
            },
        );
    }

    fn configure_event_settings(&self, source: &str, mut settings: EventSettings) {
        if !settings_valid(source, settings) {
            self.reject_event_settings(source, &mut settings, "invalid event configuration");
        }
        let mut policies = self
            .admission
            .policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut events = self
            .admission
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if enabled(settings) && !events.contains_key(source) && events.len() >= 127 {
            self.reject_event_settings(source, &mut settings, "event camera limit reached");
        }
        let (generations, queued_frames) = events.remove(source).map_or_else(
            || {
                (
                    Arc::new(EventGenerations::default()),
                    Arc::new(AtomicUsize::new(0)),
                )
            },
            |entry| {
                entry.generations.source.fetch_add(1, Ordering::AcqRel);
                (entry.generations, entry.queued_frames)
            },
        );
        let entry = EventAdmission {
            settings,
            generations,
            queued_frames,
        };
        let fence = self.admission.event_fence(source, &entry);
        if enabled(settings) {
            events.insert(source.to_owned(), entry);
        }
        self.admission
            .events_enabled
            .store(!events.is_empty(), Ordering::Release);
        policies.insert(
            source.to_owned(),
            CameraRecordingPolicy::new(settings.mode, settings.post),
        );
        self.tx.send_control(Command::ConfigureEventRecording {
            source: source.to_owned(),
            settings,
            fence,
        });
    }

    fn reject_event_settings(&self, source: &str, settings: &mut EventSettings, reason: &str) {
        self.admission.health.note_failure(source, reason);
        settings.mode = CameraRecordingMode::Off;
        settings.pre = Duration::ZERO;
        settings.post = Duration::from_secs(1);
    }

    pub fn reset_event_recording(&self, source: &str) {
        if !self.admission.has_event_recording(source) {
            return;
        }
        let _policies = self
            .admission
            .policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let events = self
            .admission
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = events.get(source) else {
            return;
        };
        entry.generations.source.fetch_add(1, Ordering::AcqRel);
        let fence = self.admission.event_fence(source, entry);
        // The shared generation also fences old output if the command queue is full.
        let _ = self.tx.tx.try_send(Command::ResetEventStream {
            source: source.to_owned(),
            fence,
        });
    }

    pub fn retains_event_audio(&self, source: &str, stream: &str) -> bool {
        if !self.admission.has_event_recording(source) {
            return false;
        }
        self.admission
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(source)
            .is_some_and(|entry| {
                entry.settings.mode == CameraRecordingMode::EventBoost
                    || stream == stream_name(entry.settings.stream)
            })
    }
}

impl WriterWorker {
    pub(super) fn handle_event_command(&mut self, command: Command) {
        match command {
            Command::ConfigureEventRecording {
                source,
                settings,
                fence,
            } => {
                self.configure_event_runtime(source, settings, fence);
            }
            Command::EventInput {
                identity,
                input,
                fence,
                main_slot,
            } => {
                drop(main_slot);
                if self.sync_event_source(&identity.source_id)
                    && fence.valid()
                    && (identity.stream_id != "main" || fence.main_valid())
                    && let Some(recordings) = &mut self.event_recordings
                {
                    recordings.ingest(identity, *input, Instant::now());
                }
            }
            Command::RecordingEvent { source, at, fence } => {
                if self.sync_event_source(&source)
                    && fence.valid()
                    && let Some(recordings) = &mut self.event_recordings
                {
                    recordings.note_event(&source, at);
                }
            }
            Command::ResetEventStream { source, fence } => {
                if fence.generations.source.load(Ordering::Acquire) == fence.observed {
                    self.invalidate_event_source(&source, PreRecordReason::Discontinuity);
                    if let Some(state) = self.event_sources.get_mut(&source) {
                        state.fence = fence;
                    }
                }
            }
            _ => unreachable!("legacy writer commands are handled before event dispatch"),
        }
    }

    fn configure_event_runtime(
        &mut self,
        source: String,
        settings: EventSettings,
        fence: EventFence,
    ) {
        if fence.generations.source.load(Ordering::Acquire) != fence.observed {
            return;
        }
        if enabled(settings) || self.event_sources.contains_key(&source) {
            self.cancel_event_writer(&source);
        }
        if enabled(settings) {
            let runtime = self.event_recordings.get_or_insert_with(|| {
                EventRecordings::new(
                    self.config.pre_recording_stream_max_bytes,
                    self.config.pre_recording_global_max_bytes,
                )
            });
            if !runtime.configure(&source, settings) {
                self.health
                    .note_failure(&source, "event recording configuration was rejected");
                return;
            }
            self.event_sources.insert(
                source.clone(),
                EventSourceState {
                    fence,
                    privacy_epoch: u64::MAX,
                    privacy_active: false,
                    paused: false,
                },
            );
            self.sync_event_source(&source);
        } else {
            if let Some(runtime) = &mut self.event_recordings {
                runtime.configure(&source, settings);
            }
            self.event_sources.remove(&source);
            self.health.clear_pre_recording(&source);
            if self.event_sources.is_empty() {
                self.event_recordings = None;
            }
        }
    }

    fn sync_event_source(&mut self, source: &str) -> bool {
        let (private, epoch) = self
            .privacy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map_or((false, 0), |registry| {
                registry
                    .decision(source, chrono::Utc::now())
                    .unwrap_or((true, u64::MAX))
            });
        let paused = self.safety.recording_paused();
        let Some(state) = self.event_sources.get_mut(source) else {
            return false;
        };
        let generation = state.fence.generations.source.load(Ordering::Acquire);
        let main_generation = state.fence.generations.main.load(Ordering::Acquire);
        let main_changed = main_generation != state.fence.main_observed;
        let privacy_changed = (state.privacy_epoch != u64::MAX && epoch != state.privacy_epoch)
            || private != state.privacy_active;
        let changed =
            generation != state.fence.observed || privacy_changed || paused != state.paused;
        state.fence.observed = generation;
        state.fence.main_observed = main_generation;
        state.privacy_epoch = epoch;
        state.privacy_active = private;
        state.paused = paused;
        if changed {
            let reason = if private || privacy_changed {
                PreRecordReason::Privacy
            } else if paused {
                PreRecordReason::StoragePause
            } else {
                PreRecordReason::Discontinuity
            };
            self.invalidate_event_source(source, reason);
        } else if main_changed && let Some(runtime) = &mut self.event_recordings {
            runtime.drop_main_history(source);
        }
        !private && !paused
    }

    fn invalidate_event_source(&mut self, source: &str, reason: PreRecordReason) {
        if let Some(runtime) = &mut self.event_recordings {
            runtime.fail(source, reason);
        }
        self.cancel_event_writer(source);
    }

    fn cancel_event_writer(&mut self, source: &str) {
        let keys: Vec<_> = self
            .pipelines
            .iter()
            .filter(|(_, p)| p.identity.source_id == source)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            let Some(mut pipeline) = self.pipelines.remove(&key) else {
                continue;
            };
            let Some(mut writer) = pipeline.medium_term.take() else {
                continue;
            };
            let id = writer.recording_id().to_owned();
            let result = writer.discard_pending().and_then(|()| writer.finalize());
            match result {
                Ok(path) if path.exists() => {
                    if let Err(error) = self.move_to_long_term_for_source(
                        &key,
                        &path,
                        &id,
                        Some(&pipeline.identity.source_id),
                    ) {
                        self.health.note_failure(&key, &error.to_string());
                    }
                }
                Ok(_) => {}
                Err(error) => self.health.note_failure(&key, &error.to_string()),
            }
        }
    }

    pub(super) fn drain_event_outputs(&mut self, now: Instant) -> usize {
        if self.event_recordings.is_none() {
            return 0;
        }
        if self
            .event_tick
            .is_none_or(|tick| now.saturating_duration_since(tick) >= Duration::from_millis(25))
        {
            self.event_tick = Some(now);
            let sources: Vec<_> = self.event_sources.keys().cloned().collect();
            for source in sources {
                self.sync_event_source(&source);
                if let Some(status) = self
                    .event_recordings
                    .as_mut()
                    .and_then(|runtime| runtime.status(&source, now))
                {
                    self.health.note_pre_recording(&source, status);
                }
            }
        }
        for count in 0..32 {
            let Some(output) = self
                .event_recordings
                .as_mut()
                .and_then(|runtime| runtime.next_output(now))
            else {
                return count;
            };
            match output {
                EventOutput::Frame { identity, frame } => self.write_event_frame(identity, frame),
                EventOutput::Finish { identity, end } => self.finish_event_recording(identity, end),
            }
        }
        32
    }

    fn write_event_frame(&mut self, identity: RecordingStreamIdentity, frame: RecordingFrame) {
        if !self.event_output_allowed(&identity.source_id) {
            return;
        }
        let source = identity.source_id.clone();
        let key = identity.storage_key.clone();
        self.pipeline_for(identity);
        self.health.note_attempt(&key);
        match self.write_frame_to_pipeline(&key, frame, true) {
            Ok(progress) if progress.wrote => {
                self.health.note_progress(&key, progress.recorded_duration);
            }
            Ok(_) => {}
            Err(error) => {
                self.health.note_failure(&key, &error.to_string());
                self.invalidate_event_source(&source, PreRecordReason::WriterFailure);
            }
        }
    }

    fn finish_event_recording(&mut self, identity: RecordingStreamIdentity, end: Instant) {
        if !self.event_output_allowed(&identity.source_id) {
            return;
        }
        let Some(mut pipeline) = self.pipelines.remove(&identity.storage_key) else {
            return;
        };
        let Some(writer) = pipeline.medium_term.take() else {
            return;
        };
        let id = writer.recording_id().to_owned();
        let result = writer.finalize_before(end).and_then(|path| {
            self.move_to_long_term_for_source(
                &identity.storage_key,
                &path,
                &id,
                Some(&identity.source_id),
            )
        });
        if let Err(error) = result {
            self.health
                .note_failure(&identity.storage_key, &error.to_string());
            self.invalidate_event_source(&identity.source_id, PreRecordReason::WriterFailure);
        }
    }

    fn event_output_allowed(&mut self, source: &str) -> bool {
        let before = self
            .event_sources
            .get(source)
            .map(|state| (state.fence.observed, state.privacy_epoch));
        self.sync_event_source(source)
            && before
                == self
                    .event_sources
                    .get(source)
                    .map(|state| (state.fence.observed, state.privacy_epoch))
    }

    pub(super) fn stop_event_recordings(&mut self) {
        let now = Instant::now();
        let sources: Vec<_> = self.event_sources.keys().cloned().collect();
        for source in sources {
            self.sync_event_source(&source);
        }
        if let Some(runtime) = &mut self.event_recordings {
            runtime.begin_shutdown(now);
        }
        // ponytail: Drain the existing writer on shutdown without another replay queue.
        // History and ingress hold at most 261120 frames, plus 127 finalizers.
        for _ in 0..8192 {
            if self.drain_event_outputs(now) < 32 {
                break;
            }
        }
        self.event_recordings = None;
        self.event_sources.clear();
    }
}
