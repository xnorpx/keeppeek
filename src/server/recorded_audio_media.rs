//! Delivers bounded corrected fragments while pinning their video and audio sources.

use super::*;
use crate::storage::playback::{AudioTimingEvidence, recorded};

const WORKERS_MAX: usize = 2;
const PERIODS_MAX: usize = 32;
const BYTES_MAX: u64 = 64 * 1024 * 1024;
const COVERED_MS_MAX: i64 = 40_000;

type Key = (String, u64);
type InitializationKey = (String, u64, u64);

struct Permit<'a>(&'a AtomicUsize);

impl<'a> Permit<'a> {
    fn acquire(workers: &'a AtomicUsize) -> Result<Self, ControlCommandError> {
        workers
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                if count < WORKERS_MAX {
                    Some(count + 1)
                } else {
                    None
                }
            })
            .map_err(|_| rejected(429, "Too many recorded audio corrections are running."))?;
        Ok(Self(workers))
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let previous = self.0.fetch_sub(1, Ordering::AcqRel);
        assert!(
            previous > 0,
            "recorded audio worker permit must have an owner"
        );
    }
}

struct LoadedPeriod {
    source: CatalogMediaFragment,
    initialization: Arc<Vec<u8>>,
    fragment: Vec<u8>,
}

impl LoadedPeriod {
    fn period(&self) -> recorded::Period<'_> {
        recorded::Period {
            initialization: self.initialization.as_slice(),
            fragment: &self.fragment,
            recording_started_at_ms: self.source.recording_started_at_ms,
        }
    }
}

struct Correction<'a> {
    state: &'a ServerState,
    request: &'a StoredMediaBatchRequest<'a>,
    origin_ms: i64,
    offset_ms: i32,
    work: &'a recorded::Work<'a>,
}

pub(super) fn batch(
    state: &ServerState,
    request: &StoredMediaBatchRequest<'_>,
    selected: &CatalogMediaFragment,
    delivery_end_ms: i64,
    playback_origin_ms: i64,
    timing: &proto::RecordedAudioTiming,
) -> Result<StoredMediaBatch, ControlCommandError> {
    let _permit = Permit::acquire(&state.recorded_audio_workers)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    check_access(state, request.source_id, deadline)?;
    validate_span(selected.start_ms, delivery_end_ms)?;
    let catalog = state
        .catalog
        .as_ref()
        .ok_or_else(|| unavailable("Recording catalog is unavailable."))?;
    let (video, _video_lease) = catalog
        .leased_media_fragments_in_range_bounded(
            request.recording_stream_id,
            selected.start_ms,
            delivery_end_ms,
            PERIODS_MAX as u32,
        )
        .map_err(|error| stored_catalog_error("query corrected video window", error))?;
    let (covered_start, covered_end) = covered_window(&video)?;
    let audio_start = covered_start
        .checked_sub(i64::from(timing.applied_offset_ms))
        .ok_or_else(|| rejected(422, "Corrected audio source window is out of range."))?;
    let audio_end = covered_end
        .checked_sub(i64::from(timing.applied_offset_ms))
        .ok_or_else(|| rejected(422, "Corrected audio source window is out of range."))?;
    check_access(state, request.source_id, deadline)?;
    let (audio, _audio_lease) = catalog
        .leased_media_fragments_in_range_bounded(
            request.recording_stream_id,
            audio_start,
            audio_end,
            PERIODS_MAX as u32,
        )
        .map_err(|error| stored_catalog_error("query corrected audio neighbors", error))?;
    let sources = source_union(&video, &audio)?;
    preflight_bytes(&sources)?;
    let cancelled = || {
        state.maintenance_active.load(Ordering::Acquire)
            || privacy_active_for_media(state, request.source_id).unwrap_or(true)
    };
    let work = recorded::Work {
        deadline,
        cancelled: &cancelled,
    };
    let correction = Correction {
        state,
        request,
        origin_ms: playback_origin_ms,
        offset_ms: timing.applied_offset_ms,
        work: &work,
    };
    let loaded = correction.load(sources)?;
    let context = audio.iter().map(fragment_key).collect::<Vec<_>>();
    correction.encode(&video, &context, &loaded, covered_start, covered_end)
}

fn validate_span(start: i64, end: i64) -> Result<(), ControlCommandError> {
    let span = end
        .checked_sub(start)
        .filter(|span| (1..=COVERED_MS_MAX).contains(span));
    if span.is_none() {
        return Err(rejected(
            422,
            "Corrected playback requires a positive window of at most 40 seconds.",
        ));
    }
    Ok(())
}

fn fragment_end(fragment: &CatalogMediaFragment) -> Result<i64, ControlCommandError> {
    let duration = i64::try_from(fragment.duration_ms)
        .map_err(|_| rejected(422, "Corrected video duration is out of range."))?;
    if duration == 0 {
        return Err(rejected(422, "Corrected video fragment has no duration."));
    }
    fragment
        .start_ms
        .checked_add(duration)
        .ok_or_else(|| rejected(422, "Corrected video end is out of range."))
}

fn covered_window(video: &[CatalogMediaFragment]) -> Result<(i64, i64), ControlCommandError> {
    let first = video
        .first()
        .ok_or_else(|| unavailable("Corrected video window is unavailable."))?;
    if video.len() > PERIODS_MAX {
        return Err(rejected(413, "Corrected video period limit exceeded."));
    }
    let mut end = first.start_ms;
    for fragment in video {
        if fragment.start_ms < end {
            return Err(unsupported(
                "Overlapping corrected video periods are not supported.",
            ));
        }
        end = fragment_end(fragment)?;
    }
    validate_span(first.start_ms, end)?;
    Ok((first.start_ms, end))
}

fn fragment_key(fragment: &CatalogMediaFragment) -> Key {
    (fragment.recording_id.clone(), fragment.sequence)
}

fn initialization_key(fragment: &CatalogMediaFragment) -> InitializationKey {
    (
        fragment.recording_id.clone(),
        fragment.init_offset,
        fragment.init_len,
    )
}

fn source_union(
    video: &[CatalogMediaFragment],
    audio: &[CatalogMediaFragment],
) -> Result<BTreeMap<Key, CatalogMediaFragment>, ControlCommandError> {
    let mut sources = BTreeMap::<Key, CatalogMediaFragment>::new();
    for fragment in video.iter().chain(audio) {
        let key = fragment_key(fragment);
        if let Some(previous) = sources.get(&key) {
            if previous.path != fragment.path
                || previous.recording_started_at_ms != fragment.recording_started_at_ms
                || previous.init_offset != fragment.init_offset
                || previous.init_len != fragment.init_len
                || previous.byte_offset != fragment.byte_offset
                || previous.byte_len != fragment.byte_len
                || previous.start_ms != fragment.start_ms
                || previous.duration_ms != fragment.duration_ms
            {
                return Err(unavailable(
                    "Recording metadata changed while preparing corrected playback.",
                ));
            }
        } else {
            sources.insert(key, fragment.clone());
        }
        if sources.len() > PERIODS_MAX {
            return Err(rejected(
                413,
                "Corrected playback source period limit exceeded.",
            ));
        }
    }
    Ok(sources)
}

fn preflight_bytes(
    sources: &BTreeMap<Key, CatalogMediaFragment>,
) -> Result<(), ControlCommandError> {
    let mut initializations = HashSet::new();
    let mut bytes = 0_u64;
    for source in sources.values() {
        let init_len = if initializations.insert(initialization_key(source)) {
            source.init_len
        } else {
            0
        };
        if source.init_len == 0 || source.byte_len == 0 {
            return Err(unsupported(
                "Corrected playback requires complete indexed MP4 periods.",
            ));
        }
        bytes = bytes
            .checked_add(init_len)
            .and_then(|total| total.checked_add(source.byte_len))
            .filter(|total| *total <= BYTES_MAX)
            .ok_or_else(|| rejected(413, "Corrected playback source bytes exceed 64 MiB."))?;
        source
            .init_offset
            .checked_add(source.init_len)
            .and_then(|_| source.byte_offset.checked_add(source.byte_len))
            .ok_or_else(|| unsupported("Corrected playback byte extent is out of range."))?;
    }
    Ok(())
}

fn check_access(
    state: &ServerState,
    source: &str,
    deadline: Instant,
) -> Result<(), ControlCommandError> {
    if Instant::now() >= deadline {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Timeout,
            504,
            "Recorded audio correction timed out.",
        ));
    }
    if state.maintenance_active.load(Ordering::Acquire) {
        return Err(rejected(409, "recording maintenance is running"));
    }
    if privacy_active_for_media(state, source)? {
        return Err(rejected(403, "camera privacy is active"));
    }
    Ok(())
}

impl Correction<'_> {
    fn check(&self) -> Result<(), ControlCommandError> {
        check_access(self.state, self.request.source_id, self.work.deadline)
    }

    fn read(
        &self,
        source: &CatalogMediaFragment,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>, ControlCommandError> {
        self.check()?;
        let bytes = read_stored_range_with_work(
            StoredReadRange {
                path: Path::new(&source.path),
                archive: source.archive.as_ref(),
                offset,
                length,
            },
            self.work,
        )?;
        self.check()?;
        Ok(bytes)
    }

    fn load(
        &self,
        sources: BTreeMap<Key, CatalogMediaFragment>,
    ) -> Result<BTreeMap<Key, LoadedPeriod>, ControlCommandError> {
        let mut initializations = BTreeMap::new();
        let mut loaded = BTreeMap::new();
        for (key, source) in sources {
            self.check()?;
            let init_key = initialization_key(&source);
            let initialization = if let Some(bytes) = initializations.get(&init_key) {
                Arc::clone(bytes)
            } else {
                let bytes = Arc::new(self.read(&source, source.init_offset, source.init_len)?);
                initializations.insert(init_key, Arc::clone(&bytes));
                bytes
            };
            let fragment = self.read(&source, source.byte_offset, source.byte_len)?;
            loaded.insert(
                key,
                LoadedPeriod {
                    source,
                    initialization,
                    fragment,
                },
            );
        }
        Ok(loaded)
    }

    fn encode(
        &self,
        video: &[CatalogMediaFragment],
        context: &[Key],
        loaded: &BTreeMap<Key, LoadedPeriod>,
        covered_start: i64,
        covered_end: i64,
    ) -> Result<StoredMediaBatch, ControlCommandError> {
        let audio = context
            .iter()
            .map(|key| loaded[key].period())
            .collect::<Vec<_>>();
        let mut encoded = Encoded {
            initialization_id: self.request.identifiers.initialization_id,
            sequence: self.request.identifiers.sequence,
            ..Default::default()
        };
        for fragment in video {
            self.check()?;
            let options = recorded::Options {
                audio_offset_ms: self.offset_ms,
                playback_origin_ms: self.origin_ms,
                window_start_ms: fragment.start_ms,
                window_end_ms: fragment_end(fragment)?,
            };
            let period = recorded::remux(
                loaded[&fragment_key(fragment)].period(),
                &audio,
                options,
                self.work,
            );
            self.check()?;
            let period = period.map_err(|error| {
                tracing::debug!(%error, "recorded audio correction rejected this media window");
                unsupported("Recorded audio correction requires compatible continuous AAC media within the supported limits.")
            })?;
            encoded.push(self.request, fragment, period)?;
        }
        self.check()?;
        Ok(StoredMediaBatch {
            archive: loaded
                .values()
                .any(|source| source.source.archive.is_some()),
            content_type: encoded.content_type.unwrap_or_else(|| "video/mp4".into()),
            fragment_time_ms: covered_start,
            delivered_through_ms: covered_end,
            messages: encoded.messages,
            audio_timing: encoded.evidence,
            identifiers: StoredMediaIdentifiers {
                initialization_id: encoded.initialization_id,
                sequence: encoded.sequence,
            },
        })
    }
}

#[derive(Default)]
struct Encoded {
    messages: Vec<OutboundDataMessage>,
    last_period: Option<StoredMediaPeriodKey>,
    initialization_id: u64,
    sequence: u64,
    content_type: Option<String>,
    evidence: Option<AudioTimingEvidence>,
    bytes: u64,
}

impl Encoded {
    fn push(
        &mut self,
        request: &StoredMediaBatchRequest<'_>,
        source: &CatalogMediaFragment,
        period: recorded::RemuxedFragment,
    ) -> Result<(), ControlCommandError> {
        let previous_messages = self.messages.len();
        let content_type = fragmented_mp4_content_type(&period.initialization)?;
        let key = StoredMediaPeriodKey {
            recording_id: source.recording_id.clone(),
            sample_descriptions: period.sample_descriptions,
        };
        if self.last_period.as_ref() != Some(&key) {
            self.initialization_id = self
                .initialization_id
                .checked_add(1)
                .ok_or_else(|| rejected(422, "Corrected initialization sequence overflowed."))?;
            append_initialization_messages(
                &mut self.messages,
                request.stored_media_id,
                request.generation,
                self.initialization_id,
                &content_type,
                &period.initialization,
            )?;
            self.last_period = Some(key);
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| rejected(422, "Corrected fragment sequence overflowed."))?;
        append_fragment_messages(
            &mut self.messages,
            request.stored_media_id,
            request.generation,
            self.initialization_id,
            self.sequence,
            source.start_ms,
            source.duration_ms,
            request.media_target,
            &period.fragment,
        )?;
        self.content_type.get_or_insert(content_type);
        self.evidence.get_or_insert(period.evidence);
        for message in &self.messages[previous_messages..] {
            self.bytes = self
                .bytes
                .checked_add(
                    u64::try_from(message.message.encoded_len())
                        .map_err(|_| rejected(413, "Corrected media output is too large."))?,
                )
                .filter(|bytes| *bytes <= BYTES_MAX)
                .ok_or_else(|| rejected(413, "Corrected media output exceeds 64 MiB."))?;
        }
        Ok(())
    }
}

fn rejected(status: u16, message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, status, message)
}

fn unsupported(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::UnsupportedRequest, 422, message)
}

fn unavailable(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Unavailable, 503, message)
}

#[cfg(test)]
mod tests;
