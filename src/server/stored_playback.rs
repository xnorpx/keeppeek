//! Owns stored playback generations and captured recorded-audio policy.

use super::*;

type Delivery = (proto::StoredMediaState, Vec<OutboundDataMessage>);
type Opened = (
    StoredMediaCursor,
    proto::StoredMediaState,
    Vec<OutboundDataMessage>,
);

struct OpenOptions {
    time_ms: i64,
    end_ms: Option<i64>,
    mode: proto::StoredMediaMode,
    target: DataChannelTarget,
    channel: proto::DataChannelKind,
    buffer_ms: u64,
}

#[derive(Clone)]
struct Snapshot {
    incarnation: Uuid,
    identifiers: StoredMediaIdentifiers,
    source_id: String,
    stream_id: String,
    recording_stream_id: String,
    requested_time_ms: i64,
    end_ms: Option<i64>,
    mode: proto::StoredMediaMode,
    playing: bool,
    playback_rate: f64,
    target: DataChannelTarget,
    buffer_ms: u64,
    generation: u64,
    origin_ms: i64,
    delivered_through_ms: i64,
    content_type: String,
    status: proto::StoredMediaStatus,
    timing: proto::RecordedAudioTiming,
}

pub(super) fn open(
    state: &ServerState,
    open: proto::OpenStoredMedia,
) -> Result<Opened, ControlCommandError> {
    validate_client_id(&open.stored_media_id, "stored media cursor ID")?;
    let camera = camera(state, &open)?;
    let options = options(&open)?;
    let stream = recording_stream(&camera, &open.stream_id).to_owned();
    let recording = recording_stream_id(&camera, &open.stream_id);
    let configured = recorded_audio::configured(&camera, &stream)?;
    let mut timing = recorded_audio::preview(&configured, open.recorded_audio_preview.as_ref())?;
    let mut batch = batch(
        state,
        StoredMediaBatchRequest {
            identifiers: StoredMediaIdentifiers::default(),
            stored_media_id: &open.stored_media_id,
            source_id: &open.source_id,
            stream_id: &stream,
            recording_stream_id: &recording,
            requested_time_ms: options.time_ms,
            end_time_ms: options.end_ms,
            mode: options.mode,
            media_target: options.target,
            max_buffer_ms: options.buffer_ms,
            generation: 1,
        },
        &timing,
        None,
    )?;
    if let Some(evidence) = batch.audio_timing {
        recorded_audio::observe(&mut timing, evidence);
    }
    let messages = std::mem::take(&mut batch.messages);
    let id = open.stored_media_id.clone();
    let cursor = initial_cursor(state, open, options, batch, timing, stream, recording);
    let result = proto_stored_media_state(&id, &cursor);
    Ok((cursor, result, messages))
}

fn initial_cursor(
    state: &ServerState,
    open: proto::OpenStoredMedia,
    options: OpenOptions,
    batch: StoredMediaBatch,
    timing: proto::RecordedAudioTiming,
    stream: String,
    recording: String,
) -> StoredMediaCursor {
    StoredMediaCursor {
        incarnation: Uuid::new_v4(),
        identifiers: batch.identifiers,
        recorded_audio: timing,
        archive: batch.archive,
        source_id: open.source_id,
        stream_id: stream,
        recording_stream_id: recording.clone(),
        requested_time_ms: options.time_ms,
        end_time_ms: options.end_ms,
        mode: options.mode,
        playing: open.playing,
        playback_rate: open.playback_rate,
        media_target: options.target,
        media_channel: options.channel,
        max_buffer_ms: options.buffer_ms,
        generation: 1,
        content_type: batch.content_type,
        fragment_time_ms: batch.fragment_time_ms,
        delivered_through_ms: batch.delivered_through_ms,
        status: stored_media_status(options.end_ms, batch.delivered_through_ms),
        _demand: state.recording_demand.acquire(recording),
    }
}

fn camera(
    state: &ServerState,
    open: &proto::OpenStoredMedia,
) -> Result<CameraEntry, ControlCommandError> {
    let camera = state.camera(&open.source_id).ok_or_else(missing)?;
    if !matches!(open.stream_id.as_str(), "main" | "sub")
        || !camera
            .info
            .profiles
            .iter()
            .any(|profile| profile.stream == open.stream_id)
    {
        return Err(missing());
    }
    if !open.data_payload_routes.is_empty() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::UnsupportedRequest,
            501,
            "stored timed-data playback is unavailable",
        ));
    }
    Ok(camera)
}

fn options(open: &proto::OpenStoredMedia) -> Result<OpenOptions, ControlCommandError> {
    let time_ms = required_timestamp_ms(open.timestamp.as_ref(), "stored media time")?;
    let end_ms = open
        .end_time
        .as_ref()
        .map(|end| required_timestamp_ms(Some(end), "stored media end time"))
        .transpose()?;
    if end_ms.is_some_and(|end| end <= time_ms) {
        return Err(invalid(
            "stored media end time must follow its initial time",
        ));
    }
    validate_rate(open.playback_rate)?;
    let mode = mode(open.mode)?;
    validate_playing(mode, open.playing)?;
    let (target, channel) = data_channel_target(open.media_channel)?;
    let requested = optional_duration_ms(open.max_buffer_duration.as_ref())?;
    let buffer_ms = if requested == 0 {
        DEFAULT_STORED_MEDIA_BUFFER.as_millis() as u64
    } else {
        requested
    };
    if buffer_ms > MAX_STORED_MEDIA_BUFFER.as_millis() as u64 {
        return Err(invalid("stored media buffer duration exceeds 300 seconds"));
    }
    Ok(OpenOptions {
        time_ms,
        end_ms,
        mode,
        target,
        channel,
        buffer_ms,
    })
}

impl Snapshot {
    fn capture(cursor: &StoredMediaCursor) -> Self {
        Self {
            incarnation: cursor.incarnation,
            identifiers: cursor.identifiers,
            source_id: cursor.source_id.clone(),
            stream_id: cursor.stream_id.clone(),
            recording_stream_id: cursor.recording_stream_id.clone(),
            requested_time_ms: cursor.requested_time_ms,
            end_ms: cursor.end_time_ms,
            mode: cursor.mode,
            playing: cursor.playing,
            playback_rate: cursor.playback_rate,
            target: cursor.media_target,
            buffer_ms: cursor.max_buffer_ms,
            generation: cursor.generation,
            origin_ms: cursor.fragment_time_ms,
            delivered_through_ms: cursor.delivered_through_ms,
            content_type: cursor.content_type.clone(),
            status: cursor.status,
            timing: cursor.recorded_audio.clone(),
        }
    }

    fn request<'a>(&'a self, id: &'a str) -> StoredMediaBatchRequest<'a> {
        StoredMediaBatchRequest {
            identifiers: self.identifiers,
            stored_media_id: id,
            source_id: &self.source_id,
            stream_id: &self.stream_id,
            recording_stream_id: &self.recording_stream_id,
            requested_time_ms: self.requested_time_ms,
            end_time_ms: self.end_ms,
            mode: self.mode,
            media_target: self.target,
            max_buffer_ms: self.buffer_ms,
            generation: self.generation,
        }
    }

    fn rotate(&mut self) -> Result<(), ControlCommandError> {
        self.identifiers = StoredMediaIdentifiers::default();
        self.generation = self.generation.checked_add(1).ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Internal,
                500,
                "stored media generation overflowed",
            )
        })?;
        self.timing.first_audio_pts_us = None;
        self.timing.first_video_pts_us = None;
        self.timing.stored_skew_us = None;
        self.timing.audio_present = None;
        self.timing.clipped_audio_samples = None;
        Ok(())
    }

    fn unchanged(&self, cursor: &StoredMediaCursor) -> bool {
        self.incarnation == cursor.incarnation
            && self.identifiers == cursor.identifiers
            && self.generation == cursor.generation
            && self.requested_time_ms == cursor.requested_time_ms
            && self.mode == cursor.mode
            && self.playing == cursor.playing
            && self.playback_rate == cursor.playback_rate
            && self.delivered_through_ms == cursor.delivered_through_ms
    }
}

fn load(
    state: &ServerState,
    session: SessionId,
    id: &str,
) -> Result<Snapshot, ControlCommandError> {
    let cursors = state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cursors
        .get(&(session, id.to_owned()))
        .map(Snapshot::capture)
        .ok_or_else(missing)
}

pub(super) fn seek(
    state: &ServerState,
    session: SessionId,
    seek: proto::SeekStoredMedia,
) -> Result<Delivery, ControlCommandError> {
    let before = load(state, session, &seek.stored_media_id)?;
    let mut next = before.clone();
    next.requested_time_ms =
        required_timestamp_ms(seek.timestamp.as_ref(), "stored media seek time")?;
    if next.end_ms.is_some_and(|end| next.requested_time_ms >= end) {
        return Err(missing());
    }
    next.rotate()?;
    let batch = batch(
        state,
        next.request(&seek.stored_media_id),
        &next.timing,
        None,
    )?;
    commit(
        state,
        session,
        &seek.stored_media_id,
        &before,
        next,
        Some(batch),
    )
}

pub(super) fn update(
    state: &ServerState,
    session: SessionId,
    update: proto::SetStoredMediaPlayback,
) -> Result<Delivery, ControlCommandError> {
    let before = load(state, session, &update.stored_media_id)?;
    let mut next = before.clone();
    if let Some(rate) = update.playback_rate {
        validate_rate(rate)?;
        next.playback_rate = rate;
    }
    if let Some(value) = update.mode {
        next.mode = mode(value)?;
    }
    next.playing = update.playing.unwrap_or(next.playing);
    validate_playing(next.mode, next.playing)?;
    if update.recorded_audio_preview.is_some() {
        next.timing =
            recorded_audio::preview(&before.timing, update.recorded_audio_preview.as_ref())?;
    }
    let starts_playback = before.mode == proto::StoredMediaMode::Scrub
        && next.mode == proto::StoredMediaMode::Playback;
    let preview_changes = update.recorded_audio_preview.is_some();
    let reload = starts_playback || preview_changes;
    let batch = if reload {
        if preview_changes {
            next.rotate()?;
        }
        Some(batch(
            state,
            next.request(&update.stored_media_id),
            &next.timing,
            None,
        )?)
    } else {
        None
    };
    commit(
        state,
        session,
        &update.stored_media_id,
        &before,
        next,
        batch,
    )
}

pub(super) fn refill(
    state: &ServerState,
    session: SessionId,
    refill: proto::RefillStoredMedia,
) -> Result<Delivery, ControlCommandError> {
    let playback =
        required_timestamp_ms(refill.playback_time.as_ref(), "stored media playback time")?;
    let before = load(state, session, &refill.stored_media_id)?;
    if before.status != proto::StoredMediaStatus::Active
        || before.mode != proto::StoredMediaMode::Playback
        || !before.playing
    {
        return Ok((
            stored_media_cursor_state(state, session, &refill.stored_media_id)?,
            Vec::new(),
        ));
    }
    let end = playback.saturating_add(
        i64::try_from(before.buffer_ms.min(STORED_MEDIA_TARGET_BUFFER_MS)).unwrap_or(i64::MAX),
    );
    let end = before.end_ms.map_or(end, |limit| limit.min(end));
    let batch = if end > before.delivered_through_ms {
        continuation(state, &refill.stored_media_id, &before, end)?
    } else {
        None
    };
    if batch
        .as_ref()
        .is_some_and(|batch| batch.content_type != before.content_type)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "stored media codec changed during refill",
        ));
    }
    commit(
        state,
        session,
        &refill.stored_media_id,
        &before,
        before.clone(),
        batch,
    )
}

fn commit(
    state: &ServerState,
    session: SessionId,
    id: &str,
    before: &Snapshot,
    next: Snapshot,
    batch: Option<StoredMediaBatch>,
) -> Result<Delivery, ControlCommandError> {
    let mut cursors = state
        .stored_media_cursors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cursor = cursors
        .get_mut(&(session, id.to_owned()))
        .filter(|cursor| before.unchanged(cursor))
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "stored media cursor changed during playback update",
            )
        })?;
    let reset = next.generation != before.generation || next.mode != before.mode;
    cursor.requested_time_ms = next.requested_time_ms;
    cursor.mode = next.mode;
    cursor.playing = next.playing;
    cursor.playback_rate = next.playback_rate;
    cursor.generation = next.generation;
    cursor.recorded_audio = next.timing;
    let messages = if let Some(batch) = batch {
        cursor.identifiers = batch.identifiers;
        if reset {
            cursor.archive = batch.archive;
            cursor.content_type = batch.content_type;
            cursor.fragment_time_ms = batch.fragment_time_ms;
            if let Some(evidence) = batch.audio_timing {
                recorded_audio::observe(&mut cursor.recorded_audio, evidence);
            }
        }
        cursor.delivered_through_ms = batch.delivered_through_ms;
        cursor.status = stored_media_status(cursor.end_time_ms, cursor.delivered_through_ms);
        batch.messages
    } else {
        Vec::new()
    };
    Ok((proto_stored_media_state(id, cursor), messages))
}

pub(super) fn batch(
    state: &ServerState,
    request: StoredMediaBatchRequest<'_>,
    timing: &proto::RecordedAudioTiming,
    origin_ms: Option<i64>,
) -> Result<StoredMediaBatch, ControlCommandError> {
    if timing.applied_offset_ms == 0 {
        return stored_media_batch(state, request);
    }
    let catalog = state.catalog.as_ref().ok_or_else(missing)?;
    let lookup_end = request
        .requested_time_ms
        .checked_add(1)
        .ok_or_else(|| invalid("stored media time is out of range"))?;
    let (selected, _lease) = catalog
        .leased_media_fragments_in_range_bounded(
            request.recording_stream_id,
            request.requested_time_ms,
            lookup_end,
            32,
        )
        .map_err(|error| stored_catalog_error("locate corrected stored media", error))?;
    let selected = selected
        .into_iter()
        .max_by_key(|fragment| fragment.start_ms)
        .ok_or_else(missing)?;
    if request.mode == proto::StoredMediaMode::Scrub {
        return encode_stored_media_keyframe(state, request, &selected);
    }
    let end = request.requested_time_ms.saturating_add(
        i64::try_from(request.max_buffer_ms.min(STORED_MEDIA_TARGET_BUFFER_MS)).unwrap_or(i64::MAX),
    );
    let end = request.end_time_ms.map_or(end, |limit| limit.min(end));
    recorded_audio_media::batch(
        state,
        &request,
        &selected,
        end,
        origin_ms.unwrap_or(selected.start_ms),
        timing,
    )
}

fn continuation(
    state: &ServerState,
    id: &str,
    snapshot: &Snapshot,
    end: i64,
) -> Result<Option<StoredMediaBatch>, ControlCommandError> {
    if snapshot.timing.applied_offset_ms == 0 {
        let mut request = snapshot.request(id);
        request.requested_time_ms = snapshot.delivered_through_ms;
        return stored_media_continuation_batch(state, request, end);
    }
    let catalog = state.catalog.as_ref().ok_or_else(missing)?;
    let (fragments, _leases) = catalog
        .leased_media_fragments_in_range_bounded(
            &snapshot.recording_stream_id,
            snapshot.delivered_through_ms,
            end,
            32,
        )
        .map_err(|error| stored_catalog_error("refill corrected stored media", error))?;
    let Some(selected) = fragments.first() else {
        return Ok(None);
    };
    let request = snapshot.request(id);
    recorded_audio_media::batch(
        state,
        &request,
        selected,
        end,
        snapshot.origin_ms,
        &snapshot.timing,
    )
    .map(Some)
}

fn mode(value: i32) -> Result<proto::StoredMediaMode, ControlCommandError> {
    match proto::StoredMediaMode::try_from(value) {
        Ok(mode) if mode != proto::StoredMediaMode::Unspecified => Ok(mode),
        _ => Err(invalid("stored media mode is invalid or unspecified")),
    }
}

fn validate_rate(rate: f64) -> Result<(), ControlCommandError> {
    if rate.is_finite() && rate > 0.0 {
        Ok(())
    } else {
        Err(invalid(
            "stored media playback rate must be finite and positive",
        ))
    }
}

fn validate_playing(
    mode: proto::StoredMediaMode,
    playing: bool,
) -> Result<(), ControlCommandError> {
    if mode == proto::StoredMediaMode::Scrub && playing {
        Err(invalid("stored media scrub mode must be paused"))
    } else {
        Ok(())
    }
}

fn missing() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::NotFound,
        404,
        "stored media cursor, source, stream, or timestamp was not found",
    )
}

fn invalid(message: &str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

#[cfg(test)]
mod tests;
