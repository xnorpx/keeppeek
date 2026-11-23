//! Exact frame step on an already-authorized paused playback cursor.
mod delivery;
pub(super) use delivery::Delivery;
#[cfg(test)]
mod tests;
use super::*;
use crate::storage::sample_index::{self as index, Unavailable};

pub(super) const CAPABILITY_ID: &str = "keeppeek.stored-frame-step.v1";

#[derive(Default)]
pub(super) struct CursorFrames {
    active: Option<Uuid>,
    confirmed: Option<Selected>,
    pending: Option<Selected>,
    queued: Option<Arc<AtomicBool>>,
}

impl CursorFrames {
    fn confirmed_anchor(
        &mut self,
        anchor: Option<&proto::step_stored_frame::Anchor>,
    ) -> Option<Selected> {
        let Some(proto::step_stored_frame::Anchor::SampleToken(bytes)) = anchor else {
            return None;
        };
        // Reusing the token is the client's acknowledgement that decoding and presentation worked.
        if self
            .pending
            .as_ref()
            .is_some_and(|picture| picture.token.as_bytes() == bytes.as_slice())
        {
            self.confirmed = self.pending.take();
        }
        self.confirmed
            .as_ref()
            .filter(|picture| picture.token.as_bytes() == bytes.as_slice())
            .cloned()
    }
}

impl Drop for CursorFrames {
    fn drop(&mut self) {
        if let Some(queued) = self.queued.take() {
            queued.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone)]
struct Selected {
    token: Uuid,
    generation: u64,
    catalog_entry: CatalogMediaFragment,
    witness: Witness,
    ordinal: u32,
    metadata: [u8; 32],
    picture: [u8; 32],
    stored_ns: i128,
}

struct Operation {
    state: ServerState,
    key: (SessionId, String),
    generation: u64,
    incarnation: Uuid,
    active: Uuid,
    queued: Arc<AtomicBool>,
}

impl Operation {
    fn current(&self, cursor: &StoredMediaCursor) -> bool {
        cursor.incarnation == self.incarnation
            && cursor.generation == self.generation
            && !cursor.playing
            && cursor.frames.active == Some(self.active)
    }
    fn cancelled(&self) -> bool {
        self.queued.load(Ordering::Acquire)
            || self
                .state
                .maintenance_active
                .load(std::sync::atomic::Ordering::Acquire)
            || !self
                .state
                .stored_media_cursors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&self.key)
                .is_some_and(|cursor| self.current(cursor))
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        self.state.frame_step_workers.fetch_sub(1, Ordering::AcqRel);
        let mut cursors = self
            .state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cursor) = cursors.get_mut(&self.key)
            && cursor.incarnation == self.incarnation
            && cursor.frames.active == Some(self.active)
        {
            cursor.frames.active = None;
        }
    }
}

fn rejected(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, 409, message)
}

fn unavailable(status: Unavailable) -> proto::StoredFrameStepStatus {
    use proto::StoredFrameStepStatus as S;
    match status {
        Unavailable::Timing => S::TimingUnavailable,
        Unavailable::Ambiguous => S::AmbiguousAnchor,
        Unavailable::Boundary => S::Boundary,
        Unavailable::Gap => S::Gap,
        Unavailable::Limit => S::DistanceLimit,
        Unavailable::Stale => S::Stale,
        Unavailable::Discontinuity => S::Discontinuity,
    }
}

fn time_ns(time: &prost_types::Timestamp) -> Option<i128> {
    (0..1_000_000_000)
        .contains(&time.nanos)
        .then_some(i128::from(time.seconds) * 1_000_000_000 + i128::from(time.nanos))
}

fn message_time(ns: i128) -> Result<prost_types::Timestamp, Unavailable> {
    Ok(prost_types::Timestamp {
        seconds: i64::try_from(ns.div_euclid(1_000_000_000)).map_err(|_| Unavailable::Timing)?,
        nanos: ns.rem_euclid(1_000_000_000) as i32,
    })
}

fn read_range(
    source: &CatalogMediaFragment,
    offset: u64,
    length: u64,
    work: &crate::storage::archive::target::Work<'_>,
) -> Result<Vec<u8>, Unavailable> {
    #[cfg(test)]
    tests::scale::observe_range(length);
    read_stored_range_with_work(
        StoredReadRange {
            path: Path::new(&source.path),
            archive: source.archive.as_ref(),
            offset,
            length,
        },
        work,
    )
    .map_err(|_| Unavailable::Stale)
}

fn index_fragment(
    source: &CatalogMediaFragment,
    remaining: u32,
    work: &crate::storage::archive::target::Work<'_>,
) -> Result<index::Fragment, Unavailable> {
    if source.init_len == 0 || source.init_len > 1_024 * 1_024 || source.byte_len < 16 {
        return Err(Unavailable::Limit);
    }
    let mut metadata = read_range(source, source.init_offset, source.init_len, work)?;
    let init_len = metadata.len();
    let mut position = 0_u64;
    // The indexed fragment starts at moof. The provenance record may appear in emsg before mdat.
    for box_index in 0..64 {
        let offset = source
            .byte_offset
            .checked_add(position)
            .ok_or(Unavailable::Stale)?;
        if source.byte_len.saturating_sub(position) < 8 {
            return Err(Unavailable::Timing);
        }
        let header = read_range(source, offset, 8, work)?;
        let kind = &header[4..8];
        let size = u32::from_be_bytes(header[..4].try_into().map_err(|_| Unavailable::Timing)?);
        metadata.extend_from_slice(&header);
        let (size, header_len) = if size == 1 {
            let extended = read_range(
                source,
                offset.checked_add(8).ok_or(Unavailable::Stale)?,
                8,
                work,
            )?;
            let length = u64::from_be_bytes(
                extended
                    .as_slice()
                    .try_into()
                    .map_err(|_| Unavailable::Timing)?,
            );
            metadata.extend_from_slice(&extended);
            (length, 16)
        } else {
            (u64::from(size), 8)
        };
        let end = position.checked_add(size).ok_or(Unavailable::Timing)?;
        if size < header_len || end > source.byte_len {
            return Err(Unavailable::Timing);
        }
        if kind == b"mdat" {
            if box_index == 0 || end != source.byte_len {
                return Err(Unavailable::Timing);
            }
            let result = index::fragment(
                &metadata,
                init_len,
                source.byte_len,
                source.recording_started_at_ms,
                work.deadline,
                remaining,
            )
            .map_err(|_| Unavailable::Timing)?;
            if u64::from(result.sequence) != source.sequence {
                return Err(Unavailable::Stale);
            }
            return Ok(result);
        }
        if (box_index == 0 && kind != b"moof") || (box_index > 0 && kind != b"emsg") {
            return Err(Unavailable::Timing);
        }
        if end > index::MAX_MOOF_BYTES as u64 {
            return Err(Unavailable::Limit);
        }
        if size > header_len {
            metadata.extend_from_slice(&read_range(
                source,
                offset.checked_add(header_len).ok_or(Unavailable::Stale)?,
                size - header_len,
                work,
            )?);
        }
        position = end;
    }
    Err(Unavailable::Limit)
}

struct Window<'a> {
    fragment: &'a CatalogMediaFragment,
    index: index::Fragment,
    presentation_order: Vec<usize>,
    target: usize,
    witness: Witness,
}

fn fragment_end(fragment: &CatalogMediaFragment) -> Result<i64, Unavailable> {
    fragment
        .start_ms
        .checked_add(i64::try_from(fragment.duration_ms).map_err(|_| Unavailable::Timing)?)
        .ok_or(Unavailable::Timing)
}

fn target_window<'a>(
    fragments: &'a [CatalogMediaFragment],
    anchor: i128,
    previous: Option<&Selected>,
    forward: bool,
    work: &crate::storage::archive::target::Work<'_>,
) -> Result<Window<'a>, Unavailable> {
    let ms = i64::try_from(anchor.div_euclid(1_000_000)).map_err(|_| Unavailable::Stale)?;
    let matches = |f: &CatalogMediaFragment| {
        previous.map_or_else(
            || f.start_ms <= ms && fragment_end(f).is_ok_and(|end| ms < end),
            |p| p.catalog_entry == *f,
        )
    };
    let mut found = fragments.iter().enumerate().filter(|(_, f)| matches(f));
    let (position, fragment) = found.next().ok_or(Unavailable::Stale)?;
    if found.next().is_some() {
        return Err(Unavailable::Ambiguous);
    }
    let current_file = witness(fragment)?;
    let current = index_fragment(fragment, index::MAX_SAMPLES, work)?;
    let displayed = if let Some(p) = previous {
        if p.metadata != current.fingerprint || !p.witness.same_source(&current_file) {
            return Err(Unavailable::Stale);
        }
        let i = usize::try_from(p.ordinal)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .ok_or(Unavailable::Stale)?;
        let sample = current.pictures.get(i).ok_or(Unavailable::Stale)?;
        let bytes = picture_payload(fragment, sample, work)?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != p.picture {
            return Err(Unavailable::Stale);
        }
        i
    } else {
        current.displayed(anchor)?
    };
    let mut inspection_bytes = index::MAX_ENCODED_BYTES;
    let mut ordered = |source: &CatalogMediaFragment, index: &index::Fragment| {
        index.presentation_order(|picture| {
            inspection_bytes = inspection_bytes
                .checked_sub(u64::from(picture.size))
                .ok_or(Unavailable::Limit)?;
            picture_payload(source, picture, work)
        })
    };
    let order = ordered(fragment, &current)?;
    let at = order
        .iter()
        .position(|i| *i == displayed)
        .ok_or(Unavailable::Stale)?;
    let next = if forward {
        at.checked_add(1)
    } else {
        at.checked_sub(1)
    };
    if let Some(&target) = next.and_then(|n| order.get(n)) {
        return Ok(Window {
            fragment,
            index: current,
            presentation_order: order,
            target,
            witness: current_file,
        });
    }
    let neighbor = if forward {
        fragments.get(position + 1)
    } else {
        position.checked_sub(1).and_then(|p| fragments.get(p))
    }
    .ok_or(Unavailable::Boundary)?;
    let (left, right) = if forward {
        (fragment, neighbor)
    } else {
        (neighbor, fragment)
    };
    let separation = right.start_ms.saturating_sub(fragment_end(left)?);
    if separation.abs_diff(0) > 1 {
        return Err(Unavailable::Gap);
    }
    let remaining = index::MAX_SAMPLES
        .saturating_sub(u32::try_from(current.pictures.len()).map_err(|_| Unavailable::Limit)?);
    if witness(fragment)? != current_file {
        return Err(Unavailable::Stale);
    }
    if remaining == 0 {
        return Err(Unavailable::Limit);
    }
    let adjacent_file = witness(neighbor)?;
    let adjacent = index_fragment(neighbor, remaining, work)?;
    let order = ordered(neighbor, &adjacent)?;
    let target =
        *if forward { order.first() } else { order.last() }.ok_or(Unavailable::Boundary)?;
    let old = current.pictures[displayed]
        .source
        .ok_or(Unavailable::Timing)?;
    let new = adjacent.pictures[target]
        .source
        .ok_or(Unavailable::Timing)?;
    // A new MP4 starts a fresh per-file calibration even when the camera clock continues.
    // Only its first physical media fragment can carry that marker across a file boundary.
    let first_of_new_file = left.recording_id != right.recording_id
        && left.path != right.path
        && right.init_offset.checked_add(right.init_len) == Some(right.byte_offset);
    if old.session_id != new.session_id
        || old.epoch_id != new.epoch_id
        || old.clock_rate != new.clock_rate
        || (fragment.recording_id == neighbor.recording_id && old.anchor != new.anchor)
        || (if forward {
            adjacent.pictures[0].source
        } else {
            current.pictures[0].source
        })
        .is_some_and(|p| p.discontinuity && !first_of_new_file)
    {
        return Err(Unavailable::Discontinuity);
    }
    // Across two independently indexed GOPs, source clocks must establish the order.
    // A new codec description is safe because every transfer carries its own config
    // and starts decoding at an independently decodable picture.
    match old.timestamp.cmp(&new.timestamp) {
        std::cmp::Ordering::Equal => return Err(Unavailable::Ambiguous),
        std::cmp::Ordering::Less if !forward => return Err(Unavailable::Discontinuity),
        std::cmp::Ordering::Greater if forward => return Err(Unavailable::Discontinuity),
        _ => {}
    }
    Ok(Window {
        fragment: neighbor,
        index: adjacent,
        presentation_order: order,
        target,
        witness: adjacent_file,
    })
}

#[derive(Clone, PartialEq, Eq)]
enum Witness {
    Local(String, u64, std::time::SystemTime),
    Archive(String, String, [u8; 32]),
}

impl Witness {
    fn same_source(&self, current: &Self) -> bool {
        match (self, current) {
            (Self::Local(id, len, _), Self::Local(other, new_len, _)) if new_len > len => {
                id == other
            }
            _ => self == current,
        }
    }
}

fn witness(source: &CatalogMediaFragment) -> Result<Witness, Unavailable> {
    if let Some(archive) = source.archive.as_ref() {
        return Ok(Witness::Archive(
            archive.object_id.clone(),
            archive.version.clone(),
            archive.digest,
        ));
    }
    let path = Path::new(&source.path);
    let metadata = std::fs::metadata(path).map_err(|_| Unavailable::Stale)?;
    Ok(Witness::Local(
        crate::storage::catalog::recording_file_identity(path, &metadata)
            .ok_or(Unavailable::Stale)?,
        metadata.len(),
        metadata.modified().map_err(|_| Unavailable::Stale)?,
    ))
}

fn picture_payload(
    fragment: &CatalogMediaFragment,
    picture: &index::Picture,
    work: &crate::storage::archive::target::Work<'_>,
) -> Result<Vec<u8>, Unavailable> {
    if u64::from(picture.size) > index::MAX_ENCODED_BYTES {
        return Err(Unavailable::Limit);
    }
    read_range(
        fragment,
        fragment
            .byte_offset
            .checked_add(picture.offset)
            .ok_or(Unavailable::Stale)?,
        u64::from(picture.size),
        work,
    )
}

const FIRST_RESPONSE: Duration = Duration::from_secs(2);
const DECODE_WINDOW: Duration = Duration::from_secs(5);
const MAX_FRAME_STEP_WORKERS: usize = 4;

struct Plan {
    result: proto::StoredFrameStepResult,
    selected: Selected,
    samples: Vec<index::Picture>,
    ranks: Vec<u32>,
}

fn plan(
    window: Window<'_>,
    request: &proto::StepStoredFrame,
    work: &crate::storage::archive::target::Work<'_>,
) -> Result<Plan, Unavailable> {
    let Window {
        fragment,
        index,
        presentation_order: order,
        target,
        witness,
    } = window;
    let target_picture = &index.pictures[target];
    let clock = target_picture.source.ok_or(Unavailable::Timing)?;
    let source_ns = target_picture.presentation_ns.ok_or(Unavailable::Timing)?;
    let first = (0..=target)
        .rev()
        .find(|&i| index.pictures[i].sync)
        .ok_or(Unavailable::Limit)?;
    // Stop before the next independently decodable group. Flushing the bounded GOP lets
    // the decoder return a B-picture whose reference was submitted earlier in decode order.
    let end = ((target + 1)..index.pictures.len())
        .find(|&i| index.pictures[i].sync)
        .unwrap_or(index.pictures.len());
    let samples = &index.pictures[first..end];
    let mut ranks = vec![0_u32; samples.len()];
    for (rank, &i) in order
        .iter()
        .filter(|i| (first..end).contains(i))
        .enumerate()
    {
        ranks[i - first] = rank as u32
    }
    let mut bytes = 0_u64;
    let mut min = i128::MAX;
    let mut max = i128::MIN;
    for p in samples {
        bytes = bytes
            .checked_add(u64::from(p.size))
            .filter(|v| *v <= index::MAX_ENCODED_BYTES)
            .ok_or(Unavailable::Limit)?;
        let ns = p.presentation_ns.ok_or(Unavailable::Timing)?;
        min = min.min(ns);
        max = max.max(ns);
    }
    if max - min > index::MAX_SPAN_NS {
        return Err(Unavailable::Limit);
    }
    // Only the selected picture's identity is needed to answer the control request.
    // Read the rest of the bounded GOP after that response has reached the transport.
    let target_digest = Sha256::digest(picture_payload(fragment, target_picture, work)?).into();
    let selected = Selected {
        token: Uuid::new_v4(),
        generation: request.generation,
        catalog_entry: fragment.clone(),
        witness,
        ordinal: target_picture.ordinal,
        metadata: index.fingerprint,
        picture: target_digest,
        stored_ns: target_picture.stored_ns,
    };
    let decoder = index.decoder;
    let result = proto::StoredFrameStepResult {
        stored_media_id: request.stored_media_id.clone(),
        generation: request.generation,
        step_id: request.step_id.clone(),
        status: proto::StoredFrameStepStatus::Ready as i32,
        sample_token: selected.token.as_bytes().to_vec(),
        presentation_time: Some(message_time(source_ns)?),
        source_ticks: clock.timestamp,
        source_timescale: clock.clock_rate.get(),
        stored_time: Some(message_time(target_picture.stored_ns)?),
        target_presentation_rank: ranks[target - first],
        sample_count: samples.len() as u32,
        encoded_bytes: bytes as u32,
        configuration: Some(proto::MediaDataConfiguration {
            stream_binding_id: format!("stored:{}:video", request.stored_media_id),
            codec: Some(proto::CodecDescriptor {
                name: decoder.codec,
                parameters: Default::default(),
            }),
            format: Some(proto::MediaDataFormat {
                format: Some(proto::media_data_format::Format::Video(
                    proto::VideoDataFormat {
                        width: u32::from(decoder.width),
                        height: u32::from(decoder.height),
                        decoder_config: decoder.description,
                    },
                )),
            }),
            configuration_revision: request.generation,
        }),
    };
    Ok(Plan {
        result,
        selected,
        samples: samples.to_vec(),
        ranks,
    })
}

impl Plan {
    fn messages(
        &self,
        work: &crate::storage::archive::target::Work<'_>,
    ) -> Result<Vec<OutboundDataMessage>, Unavailable> {
        let mut messages = Vec::new();
        for (i, picture) in self.samples.iter().enumerate() {
            let data = picture_payload(&self.selected.catalog_entry, picture, work)?;
            if picture.ordinal == self.selected.ordinal
                && <[u8; 32]>::from(Sha256::digest(&data)) != self.selected.picture
            {
                return Err(Unavailable::Stale);
            }
            let chunks = data.chunks(DATA_MESSAGE_CHUNK_BYTES);
            let total = chunks.len() as u32;
            let end = (picture.stored_end_ns > picture.stored_ns)
                .then(|| message_time(picture.stored_end_ns))
                .transpose()?;
            let mut bounds = Some((Some(message_time(picture.stored_ns)?), end));
            for (part, payload) in chunks.enumerate() {
                work.check().map_err(|_| Unavailable::Stale)?;
                let (stored_time, stored_end_time) = bounds.take().unwrap_or_default();
                let chunk = proto::StoredFrameStepChunk {
                    stored_media_id: self.result.stored_media_id.clone(),
                    generation: self.result.generation,
                    step_id: self.result.step_id.clone(),
                    decode_index: i as u32,
                    presentation_rank: self.ranks[i],
                    random_access: picture.sync,
                    chunk_index: part as u32,
                    chunk_count: total,
                    payload: payload.to_vec(),
                    sample_bytes: picture.size,
                    stored_time,
                    stored_end_time,
                };
                messages.push(OutboundDataMessage {
                    target: DataChannelTarget::Reliable,
                    group: crate::webrtc::frame_step_group(&self.result.stored_media_id),
                    message: proto::Message {
                        message: Some(proto::message::Message::StoredMedia(
                            proto::StoredMediaMessage {
                                message: Some(
                                    proto::stored_media_message::Message::FrameStepChunk(chunk),
                                ),
                            },
                        )),
                    },
                });
            }
        }
        Ok(messages)
    }
}

fn window_failure(error: anyhow::Error) -> Unavailable {
    if error.is::<crate::storage::catalog::FragmentWindowLimit>() {
        Unavailable::Limit
    } else {
        Unavailable::Stale
    }
}

struct Snapshot {
    stream: String,
    anchor_ms: i64,
    revision: u64,
    fragments: Vec<CatalogMediaFragment>,
    _leases: crate::storage::catalog::readers::LeaseSet,
}

impl Snapshot {
    fn check(
        &self,
        op: &Operation,
        selected: &Selected,
        deadline: Instant,
    ) -> Result<(), Unavailable> {
        let catalog = op.state.catalog.as_ref().ok_or(Unavailable::Timing)?;
        if catalog
            .frame_catalog_revision(deadline)
            .map_err(|_| Unavailable::Stale)?
            != self.revision
        {
            // Writes on unrelated streams can advance the revision during preparation or transfer.
            let (_, mut latest, _latest_lease) = catalog
                .frame_step_window(&self.stream, self.anchor_ms, deadline)
                .map_err(window_failure)?;
            latest.sort_by_key(|f| (f.start_ms, f.sequence));
            if latest != self.fragments {
                return Err(Unavailable::Stale);
            }
        }
        if witness(&selected.catalog_entry)? != selected.witness
            || op.cancelled()
            || Instant::now() >= deadline
        {
            return Err(Unavailable::Stale);
        }
        Ok(())
    }
}

fn run(
    op: &Operation,
    request: &proto::StepStoredFrame,
    stream: &str,
    previous: Option<&Selected>,
    bounds: CursorBounds,
    deadline: Instant,
) -> Result<(Plan, Snapshot), Unavailable> {
    use proto::step_stored_frame::Anchor;
    let (anchor, previous) = match request.anchor.as_ref() {
        Some(Anchor::DisplayedTime(time)) => {
            let anchor = time_ns(time).ok_or(Unavailable::Stale)?;
            if anchor < i128::from(bounds.origin) * 1_000_000
                || anchor >= i128::from(bounds.delivered) * 1_000_000
            {
                return Err(Unavailable::Stale);
            }
            (anchor, None)
        }
        Some(Anchor::SampleToken(bytes)) => {
            let p = previous
                .filter(|p| {
                    p.generation == request.generation && bytes.as_slice() == p.token.as_bytes()
                })
                .ok_or(Unavailable::Stale)?;
            (p.stored_ns, Some(p))
        }
        None => return Err(Unavailable::Stale),
    };
    let now = i64::try_from(anchor.div_euclid(1_000_000)).map_err(|_| Unavailable::Stale)?;
    let catalog = op.state.catalog.as_ref().ok_or(Unavailable::Timing)?;
    let (revision, mut fragments, _leases) = catalog
        .frame_step_window(stream, now, deadline)
        .map_err(window_failure)?;
    fragments.sort_by_key(|f| (f.start_ms, f.sequence));
    let cancelled = || op.cancelled();
    let work = crate::storage::archive::target::Work {
        deadline,
        cancelled: &cancelled,
    };
    work.check().map_err(|_| Unavailable::Stale)?;
    let forward = request.direction == proto::StoredFrameStepDirection::Next as i32;
    let window = target_window(&fragments, anchor, previous, forward, &work)?;
    let target_ns = window.index.pictures[window.target].stored_ns;
    if bounds
        .end
        .is_some_and(|end| target_ns >= i128::from(end) * 1_000_000)
    {
        return Err(Unavailable::Boundary);
    }
    let prepared = plan(window, request, &work)?;
    let snapshot = Snapshot {
        stream: stream.into(),
        anchor_ms: now,
        revision,
        fragments,
        _leases,
    };
    snapshot.check(op, &prepared.selected, deadline)?;
    Ok((prepared, snapshot))
}

struct CursorBounds {
    origin: i64,
    delivered: i64,
    end: Option<i64>,
}

pub(super) fn step(
    state: &ServerState,
    session: SessionId,
    request: proto::StepStoredFrame,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    step_until(state, session, request, Instant::now() + FIRST_RESPONSE)
}

fn step_until(
    state: &ServerState,
    session: SessionId,
    request: proto::StepStoredFrame,
    deadline: Instant,
) -> Result<StoredMediaDispatch, ControlCommandError> {
    validate_client_id(&request.source_id, "stored frame source ID")?;
    if !matches!(request.stream_id.as_str(), "main" | "sub") {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "stored frame stream must be main or sub",
        ));
    }
    validate_client_id(&request.stored_media_id, "stored media cursor ID")?;
    validate_client_id(&request.step_id, "stored picture request ID")?;
    if !matches!(
        proto::StoredFrameStepDirection::try_from(request.direction),
        Ok(proto::StoredFrameStepDirection::Previous | proto::StoredFrameStepDirection::Next)
    ) || request.anchor.is_none()
    {
        return Err(rejected("frame request needs a direction and anchor"));
    }
    let check_access = || {
        let access = camera_access::for_session(state, session)?;
        camera_access::require_camera(&access, &request.source_id)
    };
    check_access()?;
    let key = (session, request.stored_media_id.clone());
    let (op, stream, previous, bounds) = {
        let mut cursors = state
            .stored_media_cursors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let c = cursors
            .get_mut(&key)
            .ok_or_else(|| rejected("stored media cursor is unavailable"))?;
        if c.source_id != request.source_id
            || c.requested_stream_id != request.stream_id
            || c.generation != request.generation
            || c.playing
            || c.mode != proto::StoredMediaMode::Playback
            || c.frames.active.is_some()
        {
            return Err(rejected("frame request requires the current paused cursor"));
        }
        state
            .frame_step_workers
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_FRAME_STEP_WORKERS).then_some(n + 1)
            })
            .map_err(|_| {
                ControlCommandError::new(
                    proto::ErrorCode::Rejected,
                    429,
                    "too many active frame steps",
                )
            })?;
        let active = Uuid::new_v4();
        let queued = Arc::new(AtomicBool::new(false));
        if let Some(previous) = c.frames.queued.replace(queued.clone()) {
            previous.store(true, Ordering::Release);
        }
        c.frames.active = Some(active);
        (
            Operation {
                state: state.clone(),
                key,
                generation: c.generation,
                incarnation: c.incarnation,
                active,
                queued,
            },
            c.recording_stream_id.clone(),
            c.frames.confirmed_anchor(request.anchor.as_ref()),
            CursorBounds {
                origin: c.fragment_time_ms,
                delivered: c.delivered_through_ms,
                end: c.end_time_ms,
            },
        )
    };
    let prepared = run(&op, &request, &stream, previous.as_ref(), bounds, deadline);
    check_access()?;
    if op.cancelled() {
        return Err(rejected("stored media cursor changed"));
    }
    if Instant::now() >= deadline {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Timeout,
            504,
            "frame step selection timed out",
        ));
    }
    let (result, frame_transfer) = match prepared {
        Ok((plan, snapshot)) => {
            let result = plan.result.clone();
            (
                result,
                Some(Box::new(Delivery {
                    op,
                    plan,
                    snapshot,
                    source: request.source_id,
                })),
            )
        }
        Err(reason) => (
            proto::StoredFrameStepResult {
                stored_media_id: request.stored_media_id,
                generation: request.generation,
                step_id: request.step_id,
                status: unavailable(reason) as i32,
                ..Default::default()
            },
            None,
        ),
    };
    Ok(StoredMediaDispatch {
        result: Some(control_ok::Result::StoredFrameStepResult(result)),
        frame_transfer,
        messages: Vec::new(),
        notifications: Vec::new(),
    })
}
