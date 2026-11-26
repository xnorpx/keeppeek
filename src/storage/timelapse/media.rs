//! Bounded fragment metadata indexing, sample tokens, and decode-window reads.

use super::{TimelapseGapReason, TimelapseSample};
use crate::storage::{
    CatalogMediaFragment, RecordingCatalogHandle, archive,
    sample_index::{self, Fragment, Picture},
};
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::{Read as _, Seek as _, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

const MAX_INIT_BYTES: u64 = 1_024 * 1_024;
const MAX_FRAGMENT_PREFIX_BOXES: usize = 64;
const SAMPLE_TOKEN_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleTokenPayload {
    pub version: u8,
    pub source_id: String,
    pub stream_id: String,
    pub recording_id: String,
    pub fragment_sequence: u64,
    pub sample_ordinal: u32,
    pub actual_time_ns: i128,
    pub fragment_fingerprint: [u8; 32],
    pub source_fingerprint: String,
}

pub struct IndexedFragment {
    pub fragment: Fragment,
    pub init_bytes: Vec<u8>,
    pub metadata_bytes: Vec<u8>,
    pub codec: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub struct DecodeWindowSample {
    pub decode_index: u32,
    pub presentation_rank: u32,
    pub random_access: bool,
    pub stored_ns: i128,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct SampleDecodeWindow {
    pub decoder: mp4::Mp4VideoDecoderConfig,
    pub init_bytes: Vec<u8>,
    pub target_presentation_rank: u32,
    pub target_stored_ns: i128,
    pub samples: Vec<DecodeWindowSample>,
    pub encoded_bytes: u32,
}

pub fn encode_sample_token(payload: &SampleTokenPayload) -> anyhow::Result<Vec<u8>> {
    ensure!(
        payload.version == SAMPLE_TOKEN_VERSION,
        "unsupported sample token version"
    );
    let body = serde_json::to_vec(payload)?;
    let digest = token_digest(&body);
    let mut out = Vec::with_capacity(digest.len() + body.len());
    out.extend_from_slice(&digest);
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn decode_sample_token(token: &[u8]) -> anyhow::Result<SampleTokenPayload> {
    ensure!(
        token.len() > 32 && token.len() <= 2_048,
        "invalid sample token length"
    );
    let (digest, body) = token.split_at(32);
    ensure!(
        digest == token_digest(body),
        "sample token integrity check failed"
    );
    let payload: SampleTokenPayload =
        serde_json::from_slice(body).context("malformed sample token payload")?;
    ensure!(
        payload.version == SAMPLE_TOKEN_VERSION,
        "unsupported sample token version"
    );
    Ok(payload)
}

fn token_digest(body: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"keeppeek.timelapse.sample-token.v1\0");
    hasher.update(body);
    hasher.finalize().into()
}

pub(super) fn read_fragment_range(
    source: &CatalogMediaFragment,
    offset: u64,
    length: u64,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, TimelapseGapReason> {
    if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Err(TimelapseGapReason::Corrupt);
    }
    if let Some(archived) = source.archive.as_ref() {
        let cancelled = || cancel.load(Ordering::Acquire);
        let work = archive::target::Work {
            deadline,
            cancelled: &cancelled,
        };
        return archive::read_range(archived, offset, length, &work)
            .map_err(|_| TimelapseGapReason::Retained);
    }
    let path = Path::new(&source.path);
    let mut file = File::open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            TimelapseGapReason::Retained
        } else {
            TimelapseGapReason::Corrupt
        }
    })?;
    let file_len = file
        .metadata()
        .map_err(|_| TimelapseGapReason::Corrupt)?
        .len();
    let end = offset
        .checked_add(length)
        .ok_or(TimelapseGapReason::Corrupt)?;
    if end > file_len {
        return Err(TimelapseGapReason::Corrupt);
    }
    let len = usize::try_from(length).map_err(|_| TimelapseGapReason::Corrupt)?;
    let mut buf = vec![0u8; len];
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| TimelapseGapReason::Corrupt)?;
    file.read_exact(&mut buf)
        .map_err(|_| TimelapseGapReason::Corrupt)?;
    Ok(buf)
}

pub(super) fn index_fragment_metadata(
    source: &CatalogMediaFragment,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<IndexedFragment, TimelapseGapReason> {
    if source.init_len == 0 || source.init_len > MAX_INIT_BYTES || source.byte_len < 16 {
        return Err(TimelapseGapReason::Corrupt);
    }
    let init_bytes = read_fragment_range(
        source,
        source.init_offset,
        source.init_len,
        deadline,
        cancel,
    )?;
    let mut metadata = init_bytes.clone();
    let init_len = metadata.len();
    let mut position = 0_u64;
    for box_index in 0..MAX_FRAGMENT_PREFIX_BOXES {
        let (kind, size, header_len, offset) =
            read_prefix_box_header(source, position, &mut metadata, deadline, cancel)?;
        let end = position
            .checked_add(size)
            .ok_or(TimelapseGapReason::Corrupt)?;
        if size < header_len || end > source.byte_len {
            return Err(TimelapseGapReason::Corrupt);
        }
        if kind == *b"mdat" {
            if box_index == 0 || end != source.byte_len {
                return Err(TimelapseGapReason::Corrupt);
            }
            return finalize_indexed_fragment(source, init_bytes, metadata, init_len, deadline);
        }
        if (box_index == 0 && kind != *b"moof") || (box_index > 0 && kind != *b"emsg") {
            return Err(TimelapseGapReason::Corrupt);
        }
        if end > sample_index::MAX_MOOF_BYTES as u64 {
            return Err(TimelapseGapReason::Corrupt);
        }
        if size > header_len {
            let body_offset = offset
                .checked_add(header_len)
                .ok_or(TimelapseGapReason::Corrupt)?;
            let body =
                read_fragment_range(source, body_offset, size - header_len, deadline, cancel)?;
            metadata.extend_from_slice(&body);
        }
        position = end;
    }
    Err(TimelapseGapReason::Corrupt)
}

fn read_prefix_box_header(
    source: &CatalogMediaFragment,
    position: u64,
    metadata: &mut Vec<u8>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<([u8; 4], u64, u64, u64), TimelapseGapReason> {
    let offset = source
        .byte_offset
        .checked_add(position)
        .ok_or(TimelapseGapReason::Corrupt)?;
    if source.byte_len.saturating_sub(position) < 8 {
        return Err(TimelapseGapReason::Corrupt);
    }
    let header = read_fragment_range(source, offset, 8, deadline, cancel)?;
    let kind: [u8; 4] = header[4..8]
        .try_into()
        .map_err(|_| TimelapseGapReason::Corrupt)?;
    let raw_size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    metadata.extend_from_slice(&header);
    if raw_size == 1 {
        let ext_offset = offset.checked_add(8).ok_or(TimelapseGapReason::Corrupt)?;
        let ext = read_fragment_range(source, ext_offset, 8, deadline, cancel)?;
        let length = u64::from_be_bytes(
            ext.as_slice()
                .try_into()
                .map_err(|_| TimelapseGapReason::Corrupt)?,
        );
        metadata.extend_from_slice(&ext);
        Ok((kind, length, 16_u64, offset))
    } else {
        Ok((kind, u64::from(raw_size), 8_u64, offset))
    }
}

fn finalize_indexed_fragment(
    source: &CatalogMediaFragment,
    init_bytes: Vec<u8>,
    metadata: Vec<u8>,
    init_len: usize,
    deadline: Instant,
) -> Result<IndexedFragment, TimelapseGapReason> {
    let parsed = sample_index::fragment(
        &metadata,
        init_len,
        source.byte_len,
        source.recording_started_at_ms,
        deadline,
        sample_index::MAX_SAMPLES,
    )
    .map_err(|error| classify_parse_error(&error))?;
    if u64::from(parsed.sequence) != source.sequence {
        return Err(TimelapseGapReason::Retained);
    }
    let codec = codec_label(&parsed.decoder).ok_or(TimelapseGapReason::UnsupportedCodec)?;
    let width = u32::from(parsed.decoder.width);
    let height = u32::from(parsed.decoder.height);
    Ok(IndexedFragment {
        fragment: parsed,
        init_bytes,
        metadata_bytes: metadata,
        codec: codec.to_owned(),
        width,
        height,
    })
}

fn classify_parse_error(error: &anyhow::Error) -> TimelapseGapReason {
    let message = error.to_string();
    if message.contains("unsupported decoder configuration") {
        TimelapseGapReason::UnsupportedCodec
    } else {
        TimelapseGapReason::Corrupt
    }
}

fn codec_label(decoder: &mp4::Mp4VideoDecoderConfig) -> Option<&'static str> {
    let codec = decoder.codec.as_str();
    if codec.starts_with("avc1") || codec.starts_with("avc3") {
        Some("h264")
    } else if codec.starts_with("hvc1") || codec.starts_with("hev1") {
        Some("h265")
    } else {
        None
    }
}

pub(super) fn picture_time_ns(picture: &Picture) -> i128 {
    picture.presentation_ns.unwrap_or(picture.stored_ns)
}

pub(super) fn select_picture_in_fragment(
    indexed: &IndexedFragment,
    target_ms: i64,
    tolerance_ms: i64,
) -> Option<&Picture> {
    let target_ns = i128::from(target_ms) * 1_000_000;
    let max_ns = i128::from(target_ms.saturating_add(tolerance_ms)) * 1_000_000;
    indexed
        .fragment
        .pictures
        .iter()
        .filter(|pic| {
            let ts = picture_time_ns(pic);
            ts >= target_ns && ts <= max_ns
        })
        .min_by_key(|pic| (picture_time_ns(pic), pic.ordinal))
}

/// Reads the bounded GOP decode window ending at a planned sample while holding a catalog lease.
pub fn read_sample_decode_window(
    catalog: &RecordingCatalogHandle,
    sample: &TimelapseSample,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<SampleDecodeWindow> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "timelapse sample read cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "timelapse sample read deadline exceeded"
    );
    let _lease = catalog
        .lease_media_fragments(std::slice::from_ref(&sample.fragment))
        .context("recording fragment is no longer readable")?;
    let indexed = index_fragment_metadata(&sample.fragment, deadline, cancel)
        .map_err(|reason| anyhow::anyhow!("timelapse fragment unavailable: {reason:?}"))?;
    ensure!(
        indexed.fragment.fingerprint == sample.fragment_fingerprint,
        "recording fragment changed since timelapse plan was created"
    );
    let target_idx = indexed
        .fragment
        .pictures
        .iter()
        .position(|pic| pic.ordinal == sample.sample_ordinal)
        .context("planned sample ordinal no longer present in fragment")?;
    let gop_start = indexed.fragment.pictures[..=target_idx]
        .iter()
        .rposition(|pic| pic.sync)
        .context("no preceding random-access keyframe in fragment")?;
    let slice = &indexed.fragment.pictures[gop_start..=target_idx];
    let ranks = presentation_ranks(slice)?;
    let target_local = slice.len() - 1;
    let target_presentation_rank = ranks[target_local];
    let (samples, total_bytes) =
        read_decode_window_payloads(&sample.fragment, slice, &ranks, deadline, cancel)?;
    Ok(SampleDecodeWindow {
        decoder: indexed.fragment.decoder,
        init_bytes: indexed.init_bytes,
        target_presentation_rank,
        target_stored_ns: slice[target_local].stored_ns,
        samples,
        encoded_bytes: u32::try_from(total_bytes).context("encoded bytes overflow")?,
    })
}

fn presentation_ranks(slice: &[Picture]) -> anyhow::Result<Vec<u32>> {
    let mut order: Vec<usize> = (0..slice.len()).collect();
    order.sort_by_key(|&idx| (picture_time_ns(&slice[idx]), slice[idx].ordinal));
    let mut ranks = vec![0u32; slice.len()];
    for (rank, &idx) in order.iter().enumerate() {
        ranks[idx] = u32::try_from(rank).context("decode window rank overflow")?;
    }
    Ok(ranks)
}

fn read_decode_window_payloads(
    fragment: &CatalogMediaFragment,
    slice: &[Picture],
    ranks: &[u32],
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<(Vec<DecodeWindowSample>, u64)> {
    let mut total_bytes = 0_u64;
    let mut out = Vec::with_capacity(slice.len());
    for (local_idx, pic) in slice.iter().enumerate() {
        ensure!(
            !cancel.load(Ordering::Acquire),
            "timelapse sample read cancelled"
        );
        total_bytes = total_bytes
            .checked_add(u64::from(pic.size))
            .context("decode window size overflow")?;
        ensure!(
            total_bytes <= sample_index::MAX_ENCODED_BYTES,
            "decode window exceeds maximum encoded byte budget"
        );
        let file_offset = fragment
            .byte_offset
            .checked_add(pic.offset)
            .context("sample offset overflow")?;
        let payload =
            read_fragment_range(fragment, file_offset, u64::from(pic.size), deadline, cancel)
                .map_err(|reason| {
                    anyhow::anyhow!("timelapse sample payload unavailable: {reason:?}")
                })?;
        out.push(DecodeWindowSample {
            decode_index: u32::try_from(local_idx).context("decode index overflow")?,
            presentation_rank: ranks[local_idx],
            random_access: pic.sync,
            stored_ns: pic.stored_ns,
            payload,
        });
    }
    Ok((out, total_bytes))
}
