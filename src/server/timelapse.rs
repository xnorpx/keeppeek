//! Authorized WebRTC handlers for bounded sparse timelapse planning and lazy sample reads.

use super::*;
use crate::storage::timelapse::{
    DEFAULT_PAGE_SIZE, DEFAULT_TOLERANCE_MS, MAX_PAGE_SIZE, TimelapseCadence, TimelapseGapReason,
    TimelapseOutputPreview, TimelapsePlan, TimelapsePlanItem, TimelapsePlanRequest,
    TimelapseSample, decode_sample_token, plan_timelapse, read_sample_decode_window,
};
use std::{collections::VecDeque, sync::OnceLock};

pub(super) const CAPABILITY_ID: &str = "keeppeek.timelapse.v1";
pub(super) const EXPORT_CAPABILITY_ID: &str = "keeppeek.timelapse-export.v1";

const MAX_TOKENS_PER_FETCH: usize = 16;
const MAX_PLANS_PER_SESSION: usize = 8;
const MAX_PLANS_GLOBAL: usize = 128;
const PLAN_DEADLINE: Duration = Duration::from_secs(15);
const FETCH_DEADLINE: Duration = Duration::from_secs(20);

#[derive(Default)]
pub(super) struct Registry {
    plans: Mutex<PlanCache>,
    transfers: Mutex<HashMap<(SessionId, String), Arc<AtomicBool>>>,
    encoder_available: OnceLock<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PlanCacheKey {
    session_id: SessionId,
    source_id: String,
    stream_id: String,
    start_ms: i64,
    end_ms: i64,
    cadence: TimelapseCadence,
    tolerance_ms: i64,
    output_preview: TimelapseOutputPreview,
    source_fingerprint: String,
}

impl PlanCacheKey {
    fn from_request(
        session_id: SessionId,
        req: &TimelapsePlanRequest,
        source_fingerprint: String,
    ) -> Self {
        Self {
            session_id,
            source_id: req.source_id.clone(),
            stream_id: req.stream_id.clone(),
            start_ms: req.start_ms,
            end_ms: req.end_ms,
            cadence: req.cadence,
            tolerance_ms: req.tolerance_ms,
            output_preview: req.output_preview.clone(),
            source_fingerprint,
        }
    }
}

#[derive(Default)]
struct PlanCache {
    entries: HashMap<PlanCacheKey, Arc<TimelapsePlan>>,
    order: VecDeque<PlanCacheKey>,
}

impl Registry {
    pub(super) fn encoder_available(&self) -> bool {
        *self.encoder_available.get_or_init(|| {
            crate::storage::timelapse_encoder::probe_local_encoder("ffmpeg").available
        })
    }

    pub(super) fn close_session(&self, session_id: SessionId) {
        let mut cache = self
            .plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.entries.retain(|key, _| key.session_id != session_id);
        cache.order.retain(|key| key.session_id != session_id);
        let mut transfers = self
            .transfers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ((owner, _), flag) in transfers.iter() {
            if *owner == session_id {
                flag.store(true, Ordering::Release);
            }
        }
        transfers.retain(|(owner, _), _| *owner != session_id);
    }

    fn insert_plan(&self, key: PlanCacheKey, plan: Arc<TimelapsePlan>) {
        let mut cache = self
            .plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let std::collections::hash_map::Entry::Occupied(mut entry) =
            cache.entries.entry(key.clone())
        {
            entry.insert(plan);
            return;
        }
        let session_id = key.session_id;
        let session_count = cache
            .entries
            .keys()
            .filter(|entry| entry.session_id == session_id)
            .count();
        if session_count >= MAX_PLANS_PER_SESSION
            && let Some(pos) = cache
                .order
                .iter()
                .position(|entry| entry.session_id == session_id)
            && let Some(evicted) = cache.order.remove(pos)
        {
            cache.entries.remove(&evicted);
        }
        while cache.entries.len() >= MAX_PLANS_GLOBAL {
            if let Some(evicted) = cache.order.pop_front() {
                cache.entries.remove(&evicted);
            } else {
                break;
            }
        }
        cache.order.push_back(key.clone());
        cache.entries.insert(key, plan);
    }

    fn get_plan(&self, key: &PlanCacheKey) -> Option<Arc<TimelapsePlan>> {
        self.plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(key)
            .cloned()
    }

    fn find_plan_by_fingerprint(
        &self,
        session_id: SessionId,
        source_id: &str,
        stream_id: &str,
        fingerprint: &str,
    ) -> Option<Arc<TimelapsePlan>> {
        self.plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .iter()
            .find(|(key, _)| {
                key.session_id == session_id
                    && key.source_id == source_id
                    && key.stream_id == stream_id
                    && key.source_fingerprint == fingerprint
            })
            .map(|(_, plan)| Arc::clone(plan))
    }
}

pub(super) fn handle_plan(
    state: &ServerState,
    session_id: SessionId,
    request: proto::PlanTimelapse,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    let camera = resolve_timelapse_camera(state, &request.source_id, &request.stream_id)?;
    let catalog = state.catalog.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        )
    })?;
    let plan_req = build_plan_request(state, &camera, &request)?;
    let (expected_fp, offset) = parse_page_token(&request.page_token)?;
    let plan = resolve_or_compute_plan(state, catalog, session_id, &plan_req, expected_fp)?;
    let page_size = if request.page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        request.page_size.min(MAX_PAGE_SIZE)
    };
    let page = plan.page(offset, page_size).map_err(|error| {
        ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string())
    })?;
    let next_page_token = page
        .next_offset
        .map(|next| format!("{}:{next}", plan.source_fingerprint))
        .unwrap_or_default();
    let result = proto_plan_result(&plan, &page.items, next_page_token);
    Ok(StoredMediaDispatch {
        frame_transfer: None,
        result: Some(proto::ok::Result::TimelapsePlan(result)),
        messages: Vec::new(),
        notifications: Vec::new(),
    })
}

fn resolve_or_compute_plan(
    state: &ServerState,
    catalog: &RecordingCatalogHandle,
    session_id: SessionId,
    plan_req: &TimelapsePlanRequest,
    expected_fp: Option<&str>,
) -> Result<Arc<TimelapsePlan>, ControlCommandError> {
    if let Some(fp) = expected_fp {
        let lookup_key = PlanCacheKey::from_request(session_id, plan_req, fp.to_owned());
        if let Some(cached) = state.timelapse.get_plan(&lookup_key) {
            return Ok(cached);
        }
    }
    let cancel = AtomicBool::new(false);
    let planned = plan_timelapse(catalog, plan_req, Instant::now() + PLAN_DEADLINE, &cancel)
        .map_err(|error| {
            ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string())
        })?;
    if let Some(fp) = expected_fp
        && planned.source_fingerprint != fp
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "timelapse page token is stale or does not match this request; refresh the plan",
        ));
    }
    let cache_key =
        PlanCacheKey::from_request(session_id, plan_req, planned.source_fingerprint.clone());
    let arc = Arc::new(planned);
    state.timelapse.insert_plan(cache_key, Arc::clone(&arc));
    Ok(arc)
}

pub(super) fn handle_fetch_samples(
    state: &ServerState,
    session_id: SessionId,
    request: proto::FetchTimelapseSamples,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    validate_client_id(&request.transfer_id, "timelapse transfer ID")?;
    let (target, channel) = data_channel_target(request.channel)?;
    if target != DataChannelTarget::Reliable {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "timelapse sample fetches require reliable-data",
        ));
    }
    if request.sample_tokens.is_empty() || request.sample_tokens.len() > MAX_TOKENS_PER_FETCH {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("timelapse fetch accepts between 1 and {MAX_TOKENS_PER_FETCH} tokens"),
        ));
    }
    let camera = resolve_timelapse_camera(state, &request.source_id, &request.stream_id)?;
    let permit = derived_media_permit(state, &camera.info.id, 409)?;
    let catalog = state.catalog.as_ref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "recording catalog is unavailable",
        )
    })?;
    let cancel = register_transfer(state, session_id, &request.transfer_id)?;
    let _cleanup = TransferGuard {
        state,
        session_id,
        transfer_id: request.transfer_id.clone(),
    };
    let (delivery, messages) = collect_sample_chunks(
        state, session_id, catalog, &request, target, channel, &permit, &cancel,
    );
    Ok(StoredMediaDispatch {
        frame_transfer: None,
        result: Some(proto::ok::Result::TimelapseSampleDelivery(delivery)),
        messages,
        notifications: Vec::new(),
    })
}

pub(super) fn handle_cancel_samples(
    state: &ServerState,
    session_id: SessionId,
    cancel: proto::CancelTimelapseSamples,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    validate_client_id(&cancel.transfer_id, "timelapse transfer ID")?;
    if let Some(flag) = state
        .timelapse
        .transfers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(session_id, cancel.transfer_id))
    {
        flag.store(true, Ordering::Release);
    }
    Ok(StoredMediaDispatch {
        frame_transfer: None,
        result: None,
        messages: Vec::new(),
        notifications: Vec::new(),
    })
}

struct TransferGuard<'a> {
    state: &'a ServerState,
    session_id: SessionId,
    transfer_id: String,
}

impl Drop for TransferGuard<'_> {
    fn drop(&mut self) {
        self.state
            .timelapse
            .transfers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(self.session_id, self.transfer_id.clone()));
    }
}

fn register_transfer(
    state: &ServerState,
    session_id: SessionId,
    transfer_id: &str,
) -> Result<Arc<AtomicBool>, ControlCommandError> {
    let mut transfers = state
        .timelapse
        .transfers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = (session_id, transfer_id.to_owned());
    if transfers.contains_key(&key) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "timelapse transfer ID is already active",
        ));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    transfers.insert(key, Arc::clone(&cancel));
    Ok(cancel)
}

#[allow(clippy::too_many_arguments)]
fn collect_sample_chunks(
    state: &ServerState,
    session_id: SessionId,
    catalog: &RecordingCatalogHandle,
    request: &proto::FetchTimelapseSamples,
    target: DataChannelTarget,
    channel: proto::DataChannelKind,
    permit: &crate::privacy::MediaPermit,
    cancel: &AtomicBool,
) -> (proto::TimelapseSampleDelivery, Vec<OutboundDataMessage>) {
    let deadline = Instant::now() + FETCH_DEADLINE;
    let group = format!("timelapse:{}", request.transfer_id);
    let mut rejected = Vec::new();
    let mut messages = Vec::new();
    let mut delivered = 0_u32;
    let mut cancelled = false;
    for token in &request.sample_tokens {
        if cancel.load(Ordering::Acquire) {
            cancelled = true;
            break;
        }
        match fetch_one_sample(
            state,
            session_id,
            catalog,
            request,
            token,
            target,
            &group,
            permit,
            deadline,
            cancel,
            &mut messages,
        ) {
            SampleFetchOutcome::Delivered => delivered = delivered.saturating_add(1),
            SampleFetchOutcome::Rejected(item) => rejected.push(item),
            SampleFetchOutcome::Cancelled => {
                cancelled = true;
                break;
            }
        }
    }
    messages.push(build_sample_end_message(
        &request.transfer_id,
        delivered,
        cancelled,
        target,
        group,
        permit,
    ));
    let delivery = proto::TimelapseSampleDelivery {
        transfer_id: request.transfer_id.clone(),
        channel: channel as i32,
        accepted_count: delivered,
        rejected,
    };
    (delivery, messages)
}

enum SampleFetchOutcome {
    Delivered,
    Rejected(proto::TimelapseSampleRejection),
    Cancelled,
}

#[allow(clippy::too_many_arguments)]
fn fetch_one_sample(
    state: &ServerState,
    session_id: SessionId,
    catalog: &RecordingCatalogHandle,
    request: &proto::FetchTimelapseSamples,
    token: &[u8],
    target: DataChannelTarget,
    group: &str,
    permit: &crate::privacy::MediaPermit,
    deadline: Instant,
    cancel: &AtomicBool,
    messages: &mut Vec<OutboundDataMessage>,
) -> SampleFetchOutcome {
    if permit.deliver(|| ()).is_none() {
        return SampleFetchOutcome::Rejected(reject_token(
            token,
            proto::ErrorCode::Rejected,
            "camera privacy is active",
            proto::TimelapseGapReason::Privacy,
        ));
    }
    let sample = match lookup_sample_for_token(state, session_id, request, token) {
        Ok(sample) => sample,
        Err(rejection) => return SampleFetchOutcome::Rejected(rejection),
    };
    match read_sample_decode_window(catalog, &sample, deadline, cancel) {
        Ok(window) => {
            append_window_chunks(
                messages,
                &request.transfer_id,
                token,
                &sample,
                &window,
                target,
                group,
                permit,
            );
            SampleFetchOutcome::Delivered
        }
        Err(error) if cancel.load(Ordering::Acquire) => {
            let _ = error;
            SampleFetchOutcome::Cancelled
        }
        Err(error) => SampleFetchOutcome::Rejected(reject_token(
            token,
            proto::ErrorCode::NotFound,
            &error.to_string(),
            proto::TimelapseGapReason::Retained,
        )),
    }
}

fn build_sample_end_message(
    transfer_id: &str,
    delivered_count: u32,
    cancelled: bool,
    target: DataChannelTarget,
    group: String,
    permit: &crate::privacy::MediaPermit,
) -> OutboundDataMessage {
    OutboundDataMessage {
        permit: Some(permit.clone()),
        target,
        group,
        message: proto::Message {
            message: Some(proto::message::Message::StoredMedia(
                proto::StoredMediaMessage {
                    message: Some(proto::stored_media_message::Message::TimelapseSampleEnd(
                        proto::TimelapseSampleEnd {
                            transfer_id: transfer_id.to_owned(),
                            delivered_count,
                            cancelled,
                            error: None,
                        },
                    )),
                },
            )),
        },
    }
}

fn lookup_sample_for_token(
    state: &ServerState,
    session_id: SessionId,
    request: &proto::FetchTimelapseSamples,
    token: &[u8],
) -> Result<TimelapseSample, proto::TimelapseSampleRejection> {
    let payload = decode_sample_token(token).map_err(|error| {
        reject_token(
            token,
            proto::ErrorCode::InvalidRequest,
            &error.to_string(),
            proto::TimelapseGapReason::Corrupt,
        )
    })?;
    if payload.source_id != request.source_id || payload.stream_id != request.stream_id {
        return Err(reject_token(
            token,
            proto::ErrorCode::Rejected,
            "sample token does not match requested source or stream",
            proto::TimelapseGapReason::Missing,
        ));
    }
    let plan = state
        .timelapse
        .find_plan_by_fingerprint(
            session_id,
            &payload.source_id,
            &payload.stream_id,
            &payload.source_fingerprint,
        )
        .ok_or_else(|| {
            reject_token(
                token,
                proto::ErrorCode::NotFound,
                "timelapse plan is stale; re-plan the range",
                proto::TimelapseGapReason::Retained,
            )
        })?;
    plan.samples()
        .find(|sample| sample.sample_token.as_slice() == token)
        .cloned()
        .ok_or_else(|| {
            reject_token(
                token,
                proto::ErrorCode::NotFound,
                "timelapse sample was not found in cached plan",
                proto::TimelapseGapReason::Retained,
            )
        })
}

#[allow(clippy::too_many_arguments)]
fn append_window_chunks(
    messages: &mut Vec<OutboundDataMessage>,
    transfer_id: &str,
    token: &[u8],
    sample: &TimelapseSample,
    window: &crate::storage::timelapse::SampleDecodeWindow,
    target: DataChannelTarget,
    group: &str,
    permit: &crate::privacy::MediaPermit,
) {
    let sample_count = u32::try_from(window.samples.len()).unwrap_or(u32::MAX);
    let config = proto::MediaDataConfiguration {
        stream_binding_id: format!("timelapse:{transfer_id}:video"),
        codec: Some(proto::CodecDescriptor {
            name: window.decoder.codec.clone(),
            parameters: Default::default(),
        }),
        format: Some(proto::MediaDataFormat {
            format: Some(proto::media_data_format::Format::Video(
                proto::VideoDataFormat {
                    width: u32::from(window.decoder.width),
                    height: u32::from(window.decoder.height),
                    decoder_config: window.decoder.description.clone(),
                },
            )),
        }),
        configuration_revision: 1,
    };
    for unit in &window.samples {
        let total_chunks = u32::try_from(unit.payload.len().div_ceil(DATA_MESSAGE_CHUNK_BYTES))
            .unwrap_or(1)
            .max(1);
        for (chunk_idx, slice) in unit.payload.chunks(DATA_MESSAGE_CHUNK_BYTES).enumerate() {
            let is_first = unit.decode_index == 0 && chunk_idx == 0;
            messages.push(OutboundDataMessage {
                permit: Some(permit.clone()),
                target,
                group: group.to_owned(),
                message: proto::Message {
                    message: Some(proto::message::Message::StoredMedia(
                        proto::StoredMediaMessage {
                            message: Some(
                                proto::stored_media_message::Message::TimelapseSampleChunk(
                                    proto::TimelapseSampleChunk {
                                        transfer_id: transfer_id.to_owned(),
                                        sample_token: token.to_vec(),
                                        actual_time: Some(millis_timestamp(sample.actual_time_ms)),
                                        decode_index: unit.decode_index,
                                        presentation_rank: unit.presentation_rank,
                                        target_presentation_rank: window.target_presentation_rank,
                                        sample_count,
                                        random_access: unit.random_access,
                                        chunk_index: chunk_idx as u32,
                                        chunk_count: total_chunks,
                                        payload: slice.to_vec(),
                                        sample_bytes: u32::try_from(unit.payload.len())
                                            .unwrap_or(u32::MAX),
                                        configuration: is_first.then(|| config.clone()),
                                    },
                                ),
                            ),
                        },
                    )),
                },
            });
        }
    }
}

fn reject_token(
    token: &[u8],
    code: proto::ErrorCode,
    message: &str,
    reason: proto::TimelapseGapReason,
) -> proto::TimelapseSampleRejection {
    proto::TimelapseSampleRejection {
        sample_token: token.to_vec(),
        code: code as i32,
        message: message.to_owned(),
        reason: reason as i32,
    }
}

pub(super) fn resolve_timelapse_camera(
    state: &ServerState,
    source_id: &str,
    stream_id: &str,
) -> Result<CameraEntry, ControlCommandError> {
    let camera = state.camera(source_id).ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "timelapse source was not found",
        )
    })?;
    if !matches!(stream_id, "main" | "sub")
        || !camera
            .info
            .profiles
            .iter()
            .any(|profile| profile.stream == stream_id)
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::NotFound,
            404,
            "timelapse stream was not found",
        ));
    }
    Ok(camera)
}

pub(super) fn camera_site_timezone(state: &ServerState, camera_id: &str) -> String {
    if let Some(schedule) = state.privacy.schedule(camera_id)
        && !schedule.timezone.is_empty()
        && schedule.timezone.parse::<chrono_tz::Tz>().is_ok()
    {
        return schedule.timezone;
    }
    "UTC".to_owned()
}

fn parse_page_token(token: &str) -> Result<(Option<&str>, usize), ControlCommandError> {
    if token.is_empty() {
        return Ok((None, 0));
    }
    let Some((fingerprint, offset_str)) = token.split_once(':') else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "malformed timelapse page token",
        ));
    };
    if fingerprint.is_empty() {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "malformed timelapse page token",
        ));
    }
    let offset = offset_str.parse::<usize>().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "malformed timelapse page offset",
        )
    })?;
    Ok((Some(fingerprint), offset))
}

fn build_plan_request(
    state: &ServerState,
    camera: &CameraEntry,
    request: &proto::PlanTimelapse,
) -> Result<TimelapsePlanRequest, ControlCommandError> {
    let start_ms = required_timestamp_ms(request.start_time.as_ref(), "timelapse start time")?;
    let end_ms = required_timestamp_ms(request.end_time.as_ref(), "timelapse end time")?;
    let cadence = match &request.cadence {
        Some(proto::plan_timelapse::Cadence::Interval(duration)) => {
            let ms = duration_to_positive_ms(duration, "timelapse interval")?;
            TimelapseCadence::IntervalMs(ms)
        }
        Some(proto::plan_timelapse::Cadence::TargetSampleCount(count)) => {
            TimelapseCadence::TargetCount(*count)
        }
        None => TimelapseCadence::TargetCount(60),
    };
    let tolerance_ms = match request.tolerance.as_ref() {
        Some(duration) => {
            i64::try_from(duration_to_nonnegative_ms(duration, "timelapse tolerance")?).map_err(
                |_| {
                    ControlCommandError::new(
                        proto::ErrorCode::InvalidRequest,
                        400,
                        "timelapse tolerance overflow",
                    )
                },
            )?
        }
        None => DEFAULT_TOLERANCE_MS,
    };
    let output_preview =
        request
            .output_preview
            .as_ref()
            .map_or_else(TimelapseOutputPreview::default, |preview| {
                TimelapseOutputPreview {
                    output_fps: preview.output_fps,
                    max_width: preview.max_width,
                    max_height: preview.max_height,
                    timestamp_overlay: preview.timestamp_overlay,
                }
            });
    let privacy_intervals = if privacy_active_for_media(state, &camera.info.id)? {
        vec![(start_ms, end_ms)]
    } else {
        Vec::new()
    };
    Ok(TimelapsePlanRequest {
        source_id: request.source_id.clone(),
        stream_id: request.stream_id.clone(),
        catalog_stream_id: recording_stream_id(camera, &request.stream_id),
        start_ms,
        end_ms,
        cadence,
        tolerance_ms,
        include_open_recordings: false,
        privacy_intervals,
        output_preview,
        site_timezone: camera_site_timezone(state, &camera.info.id),
        encoder_available: state.timelapse.encoder_available(),
    })
}

pub(super) fn duration_to_positive_ms(
    duration: &prost_types::Duration,
    label: &str,
) -> Result<u64, ControlCommandError> {
    let ms = duration_to_nonnegative_ms(duration, label)?;
    if ms == 0 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{label} must be positive"),
        ));
    }
    Ok(ms)
}

pub(super) fn duration_to_nonnegative_ms(
    duration: &prost_types::Duration,
    label: &str,
) -> Result<u64, ControlCommandError> {
    if duration.seconds < 0 || !(0..1_000_000_000).contains(&duration.nanos) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            format!("{label} must not be negative"),
        ));
    }
    let secs_ms = u64::try_from(duration.seconds)
        .ok()
        .and_then(|secs| secs.checked_mul(1_000))
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("{label} overflow"),
            )
        })?;
    secs_ms
        .checked_add(u64::from((duration.nanos / 1_000_000) as u32))
        .ok_or_else(|| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                format!("{label} overflow"),
            )
        })
}

const fn proto_gap_reason(reason: TimelapseGapReason) -> proto::TimelapseGapReason {
    match reason {
        TimelapseGapReason::Missing => proto::TimelapseGapReason::Missing,
        TimelapseGapReason::Retained => proto::TimelapseGapReason::Retained,
        TimelapseGapReason::Privacy => proto::TimelapseGapReason::Privacy,
        TimelapseGapReason::UnsupportedCodec => proto::TimelapseGapReason::UnsupportedCodec,
        TimelapseGapReason::Corrupt => proto::TimelapseGapReason::Corrupt,
        TimelapseGapReason::OpenRecording => proto::TimelapseGapReason::OpenRecording,
    }
}

fn proto_plan_result(
    plan: &TimelapsePlan,
    items: &[TimelapsePlanItem],
    next_page_token: String,
) -> proto::TimelapsePlanResult {
    let proto_items = items
        .iter()
        .map(|item| match item {
            TimelapsePlanItem::Sample(sample) => proto::TimelapsePlanItem {
                entry: Some(proto::timelapse_plan_item::Entry::Sample(
                    proto::TimelapseSample {
                        index: sample.index,
                        requested_time: Some(millis_timestamp(sample.requested_time_ms)),
                        actual_time: Some(millis_timestamp(sample.actual_time_ms)),
                        source_ticks: sample.source_ticks,
                        source_timescale: sample.source_timescale,
                        recording_id: sample.recording_id.clone(),
                        fragment_sequence: sample.fragment_sequence,
                        sample_ordinal: sample.sample_ordinal,
                        codec: sample.codec.clone(),
                        width: sample.width,
                        height: sample.height,
                        encoded_bytes: sample.encoded_bytes,
                        sample_token: sample.sample_token.clone(),
                    },
                )),
            },
            TimelapsePlanItem::Gap(gap) => proto::TimelapsePlanItem {
                entry: Some(proto::timelapse_plan_item::Entry::Gap(
                    proto::TimelapseGap {
                        start_time: Some(millis_timestamp(gap.start_ms)),
                        end_time: Some(millis_timestamp(gap.end_ms)),
                        reason: proto_gap_reason(gap.reason) as i32,
                    },
                )),
            },
        })
        .collect();
    proto::TimelapsePlanResult {
        source_id: plan.source_id.clone(),
        stream_id: plan.stream_id.clone(),
        requested_start_time: Some(millis_timestamp(plan.requested_start_ms)),
        requested_end_time: Some(millis_timestamp(plan.requested_end_ms)),
        source_fingerprint: plan.source_fingerprint.clone(),
        catalog_revision: plan.catalog_revision,
        estimate: Some(proto::TimelapseEstimate {
            sample_count: plan.estimate.sample_count,
            gap_count: plan.estimate.gap_count,
            gap_duration_ms: plan.estimate.gap_duration_ms,
            output_duration_ms: plan.estimate.output_duration_ms,
            estimated_source_bytes: plan.estimate.estimated_source_bytes,
            estimated_output_bytes: plan.estimate.estimated_output_bytes,
            requires_confirmation: plan.estimate.requires_confirmation,
            encoder_available: plan.estimate.encoder_available,
            site_timezone: plan.site_timezone.clone(),
        }),
        items: proto_items,
        total_samples: plan.estimate.sample_count,
        next_page_token,
    }
}
