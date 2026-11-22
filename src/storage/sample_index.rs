//! Bounded picture metadata for one catalogued recording fragment.
mod h264_order;

use super::long_term::inspection::container;
use anyhow::{Context as _, ensure};
use sha2::{Digest as _, Sha256};
use std::{io::Cursor, time::Instant};

pub(crate) const MAX_SAMPLES: u32 = 4_096;
pub(crate) const MAX_ENCODED_BYTES: u64 = 32 * 1_024 * 1_024;
pub(crate) const MAX_SPAN_MS: i64 = 10_000;
pub(crate) const MAX_SPAN_NS: i128 = MAX_SPAN_MS as i128 * 1_000_000;
pub(crate) const MAX_MOOF_BYTES: usize = 1_024 * 1_024;

#[derive(Clone, Debug)]
pub(crate) struct Picture {
    pub ordinal: u32,
    pub stored_ns: i128,
    pub source: Option<mp4::SourceRtpTime>,
    pub presentation_ns: Option<i128>,
    pub offset: u64,
    pub size: u32,
    pub sync: bool,
}

pub(crate) struct Fragment {
    pub pictures: Vec<Picture>,
    pub decoder: mp4::Mp4VideoDecoderConfig,
    pub fingerprint: [u8; 32],
    pub sequence: u32,
}

fn ticks_to_ns(ticks: i128, rate: u32) -> anyhow::Result<i128> {
    ensure!(rate > 0, "zero recording clock rate");
    Ok(ticks
        .checked_mul(1_000_000_000)
        .context("timestamp overflow")?
        .div_euclid(i128::from(rate)))
}

/// The input stops at the start of mdat payload. The reader may seek over the logical
/// payload but never needs to read it to validate the index or locate individual pictures.
pub(crate) fn fragment(
    metadata: &[u8],
    init_len: usize,
    fragment_bytes: u64,
    start_ms: i64,
    deadline: Instant,
    remaining_samples: u32,
) -> anyhow::Result<Fragment> {
    ensure!(
        remaining_samples <= MAX_SAMPLES && remaining_samples > 0,
        "sample window limit exceeded"
    );
    ensure!(
        init_len > 0
            && init_len <= 1_024 * 1_024
            && metadata.len() > init_len
            && metadata.len() - init_len <= MAX_MOOF_BYTES + 16,
        "frame metadata limit exceeded"
    );
    let size = (init_len as u64)
        .checked_add(fragment_bytes)
        .context("fragment size overflow")?;
    ensure!(size > metadata.len() as u64, "empty fragment payload");
    let mut source = Cursor::new(metadata);
    let checked = container::inspect_with_sample_limit(
        &mut source,
        size,
        deadline,
        u64::from(remaining_samples),
    )?;
    ensure!(
        checked.initialization.size == init_len as u64 && checked.fragments.len() == 1,
        "unexpected fragment boundaries"
    );
    let checked = &checked.fragments[0];
    ensure!(
        checked.range.offset == init_len as u64 && checked.range.size == fragment_bytes,
        "fragment extent changed"
    );
    let reader = mp4::Mp4Reader::read_header(Cursor::new(metadata), size)?;
    let mut video = reader
        .tracks()
        .iter()
        .filter(|(_, track)| matches!(track.track_type(), Ok(mp4::TrackType::Video)));
    let (&track_id, track) = video.next().context("no video track")?;
    ensure!(video.next().is_none(), "multiple video tracks");
    let count = track.sample_count();
    ensure!(
        (1..=remaining_samples).contains(&count),
        "video sample limit exceeded"
    );
    let description = checked.first_sample.sample_description_index;
    let decoder = track
        .video_decoder_config_for_description(description)?
        .context("unsupported decoder configuration")?;
    let origin = i128::from(start_ms) * 1_000_000;
    let mut pictures = Vec::with_capacity(count as usize);
    for ordinal in 1..=count {
        ensure!(
            Instant::now() < deadline,
            "frame metadata deadline exceeded"
        );
        let timing = track
            .sample_timing(ordinal)?
            .context("missing picture timing")?;
        let location = reader
            .sample_location(track_id, ordinal)?
            .context("missing picture extent")?;
        let offset = location
            .offset
            .checked_sub(init_len as u64)
            .context("picture outside media fragment")?;
        ensure!(
            location.offset >= metadata.len() as u64
                && location.size > 0
                && offset
                    .checked_add(u64::from(location.size))
                    .is_some_and(|end| end <= fragment_bytes),
            "picture outside media payload"
        );
        let measured = reader.source_rtp_time(track_id, ordinal)?;
        let presentation_ns = measured
            .map(|clock| -> anyhow::Result<i128> {
                let anchored = ticks_to_ns(
                    i128::from(clock.anchor.presentation_time),
                    track.timescale(),
                )?;
                let delta = i128::from(clock.timestamp) - i128::from(clock.anchor.timestamp);
                Ok(origin + anchored + ticks_to_ns(delta, clock.clock_rate.get())?)
            })
            .transpose()?;
        pictures.push(Picture {
            ordinal,
            stored_ns: origin + ticks_to_ns(timing.presentation_time, track.timescale())?,
            source: measured,
            presentation_ns,
            offset,
            size: location.size,
            sync: track
                .sample_random_access(ordinal)
                .context("missing sample flags")?,
        });
    }
    Ok(Fragment {
        pictures,
        decoder,
        fingerprint: Sha256::digest(metadata).into(),
        sequence: checked.first_sample.sequence_number,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    Timing,
    Ambiguous,
    Boundary,
    Gap,
    Limit,
    Stale,
    Discontinuity,
}

impl Fragment {
    pub(crate) fn displayed(&self, stored_ns: i128) -> Result<usize, Unavailable> {
        // requestVideoFrameCallback exposes seconds derived from the browser's microsecond media clock.
        let micros = |ns: i128| (ns + 500).div_euclid(1_000);
        let mut found = self
            .pictures
            .iter()
            .enumerate()
            .filter(|(_, p)| micros(p.stored_ns) == micros(stored_ns));
        let (index, _) = found.next().ok_or(Unavailable::Stale)?;
        if found.next().is_some() {
            return Err(Unavailable::Ambiguous);
        }
        Ok(index)
    }

    pub(crate) fn presentation_order(
        &self,
        mut payload: impl FnMut(&Picture) -> Result<Vec<u8>, Unavailable>,
    ) -> Result<Vec<usize>, Unavailable> {
        let mut order: Vec<usize> = (0..self.pictures.len()).collect();
        let first = self
            .pictures
            .first()
            .and_then(|p| p.source)
            .ok_or(Unavailable::Timing)?;
        for (index, p) in self.pictures.iter().enumerate() {
            let clock = p.source.ok_or(Unavailable::Timing)?;
            if clock.session_id != first.session_id
                || clock.epoch_id != first.epoch_id
                || clock.clock_rate != first.clock_rate
                || clock.anchor != first.anchor
                || (index > 0 && clock.discontinuity)
            {
                return Err(Unavailable::Discontinuity);
            }
        }
        order.sort_unstable_by_key(|&index| {
            self.pictures[index].source.map(|clock| clock.timestamp)
        });
        let ticks = |i: usize| self.pictures[i].source.map(|clock| clock.timestamp);
        if order
            .windows(2)
            .all(|pair| ticks(pair[0]) != ticks(pair[1]))
        {
            return Ok(order);
        }
        let first_ns = self.pictures[*order.first().ok_or(Unavailable::Timing)?]
            .presentation_ns
            .ok_or(Unavailable::Timing)?;
        let last_ns = self.pictures[*order.last().ok_or(Unavailable::Timing)?]
            .presentation_ns
            .ok_or(Unavailable::Timing)?;
        if last_ns - first_ns > MAX_SPAN_NS {
            return Err(Unavailable::Limit);
        }
        let mut decoder =
            h264_order::PictureOrder::new(&self.decoder).ok_or(Unavailable::Ambiguous)?;
        let mut counts = Vec::with_capacity(self.pictures.len());
        let mut group = 0;
        for picture in &self.pictures {
            // Each IDR starts a new picture-order count sequence.
            let count = decoder
                .next(&payload(picture)?, picture.sync)
                .ok_or(Unavailable::Ambiguous)?;
            if picture.sync {
                group = picture.ordinal;
            }
            counts.push((group, count));
        }
        order.sort_unstable_by_key(|&i| counts[i]);
        if order.windows(2).any(|pair| {
            counts[pair[0]] == counts[pair[1]]
                || ticks(pair[0]) > ticks(pair[1])
                || (counts[pair[0]].0 != counts[pair[1]].0 && ticks(pair[0]) == ticks(pair[1]))
        }) {
            return Err(Unavailable::Ambiguous);
        }
        Ok(order)
    }
}
