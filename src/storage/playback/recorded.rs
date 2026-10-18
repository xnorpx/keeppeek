//! Remuxes one stored video fragment and bounded neighboring audio on a common cursor clock.

use super::{AudioTimingEvidence, audio_timing};
use crate::storage::long_term::inspection::container;
use anyhow::{Context as _, bail, ensure};
use std::{
    collections::BTreeMap,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
};

pub use crate::storage::archive::target::Work;

const INPUT_BYTES_MAX: usize = 64 * 1024 * 1024;
const OUTPUT_BYTES_MAX: u64 = 64 * 1024 * 1024;
const CONTEXT_PERIODS_MAX: usize = 32;
const SAMPLES_MAX: u32 = 65_536;

#[derive(Debug, Clone, Copy)]
pub struct Period<'a> {
    pub initialization: &'a [u8],
    pub fragment: &'a [u8],
    pub recording_started_at_ms: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub audio_offset_ms: i32,
    pub playback_origin_ms: i64,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
}

#[derive(Debug)]
pub struct RemuxedFragment {
    pub initialization: Vec<u8>,
    pub fragment: Vec<u8>,
    pub sample_descriptions: Vec<u32>,
    pub evidence: AudioTimingEvidence,
}

struct Track {
    id: u32,
    description: u32,
    config: mp4::TrackConfig,
}

struct BoundedBytes<'a> {
    bytes: Cursor<Vec<u8>>,
    work: Work<'a>,
}

type Reader<'a> = mp4::Mp4Reader<BoundedBytes<'a>>;

/// The caller pins and authorizes every period. Windows are half-open and must not overlap
/// between consecutive output fragments. Timing observations describe the original video period.
pub fn remux(
    video: Period<'_>,
    audio_context: &[Period<'_>],
    options: Options,
    work: &Work<'_>,
) -> anyhow::Result<RemuxedFragment> {
    validate_request(video, audio_context, options, work)?;
    let mut reader = parse(video, work)?;
    let (video_track, audio_track) = tracks(&reader)?;
    let mut remaining_samples = SAMPLES_MAX;
    reserve_samples(&reader, &mut remaining_samples)?;
    let mut evidence = observe(
        &mut reader,
        &video_track,
        &audio_track,
        video,
        options,
        work,
    )?;
    let video_samples = video_samples(&mut reader, &video_track, video, options, work)?;
    let audio_samples = audio_samples(
        audio_context,
        &audio_track,
        options,
        &mut remaining_samples,
        &mut evidence,
        work,
    )?;
    let config = mp4::Mp4Config {
        major_brand: *reader.major_brand(),
        minor_version: reader.minor_version(),
        compatible_brands: reader.compatible_brands().to_vec(),
        timescale: reader.timescale(),
    };
    let descriptions = vec![video_track.description, audio_track.description];
    let mut writer = mp4::FragmentedMp4Writer::write_start(
        BoundedBytes::new(Vec::new(), work),
        &config,
        &[video_track.config, audio_track.config],
    )?;
    let initialization = writer.initialization();
    write_samples(&mut writer, 1, video_samples, work)?;
    write_samples(&mut writer, 2, audio_samples, work)?;
    let fragment = writer
        .write_end()?
        .context("corrected fragment has no video")?;
    work.check()?;
    let mut bytes = writer.into_writer().bytes.into_inner();
    let split = usize::try_from(fragment.range.offset)?;
    ensure!(initialization.offset == 0 && initialization.size == fragment.range.offset);
    let fragment = bytes.split_off(split);
    Ok(RemuxedFragment {
        initialization: bytes,
        fragment,
        sample_descriptions: descriptions,
        evidence,
    })
}

fn validate_request(
    video: Period<'_>,
    context: &[Period<'_>],
    options: Options,
    work: &Work<'_>,
) -> anyhow::Result<()> {
    work.check()?;
    ensure!(
        context.len() <= CONTEXT_PERIODS_MAX,
        "audio context period limit exceeded"
    );
    ensure!(
        options.window_start_ms < options.window_end_ms,
        "invalid corrected video window"
    );
    ensure!(
        options.playback_origin_ms <= options.window_start_ms,
        "cursor origin exceeds window"
    );
    let mut bytes = 0_usize;
    for period in std::iter::once(&video).chain(context) {
        bytes = bytes
            .checked_add(period.initialization.len())
            .and_then(|count| count.checked_add(period.fragment.len()))
            .context("audio context size overflow")?;
        ensure!(
            bytes <= INPUT_BYTES_MAX,
            "audio context byte limit exceeded"
        );
    }
    Ok(())
}

fn parse<'a>(period: Period<'_>, work: &Work<'a>) -> anyhow::Result<Reader<'a>> {
    work.check()?;
    let size = period
        .initialization
        .len()
        .checked_add(period.fragment.len())
        .context("stored period size overflow")?;
    let mut bytes = Vec::with_capacity(size);
    bytes.extend_from_slice(period.initialization);
    bytes.extend_from_slice(period.fragment);
    let mut bounded = BoundedBytes::new(bytes, work);
    let index = container::inspect(&mut bounded, u64::try_from(size)?, work.deadline)?;
    ensure!(
        index.initialization.size == u64::try_from(period.initialization.len())?
            && index.fragments.len() == 1,
        "corrected playback requires exactly one indexed fragment"
    );
    bounded.seek(SeekFrom::Start(0))?;
    Ok(mp4::Mp4Reader::read_header(bounded, u64::try_from(size)?)?)
}

fn tracks(reader: &Reader<'_>) -> anyhow::Result<(Track, Track)> {
    ensure!(
        reader.tracks().len() == 2,
        "correction requires one video and one AAC track"
    );
    let mut video = None;
    let mut audio = None;
    for (&id, track) in reader.tracks() {
        let description = match track.sample_count() {
            0 if track.sample_description_count() == 1 => 1,
            0 => bail!("empty track has ambiguous sample descriptions"),
            _ => track.sample_description_index(1)?,
        };
        let config = mp4::TrackConfig {
            track_type: track.track_type()?,
            timescale: track.timescale(),
            language: track.language().to_owned(),
            media_conf: track.media_config_for_description(description)?,
        };
        ensure!(config.timescale != 0, "corrected track has no timescale");
        let resolved = Track {
            id,
            description,
            config,
        };
        match resolved.config.media_conf {
            mp4::MediaConfig::AvcConfig(_) | mp4::MediaConfig::HevcConfig(_) => {
                ensure!(video.is_none(), "multiple corrected video tracks");
                video = Some(resolved);
            }
            mp4::MediaConfig::AacConfig(_) => {
                ensure!(audio.is_none(), "multiple corrected audio tracks");
                audio = Some(resolved);
            }
            _ => bail!("unsupported corrected track codec"),
        }
    }
    Ok((
        video.context("corrected period has no video")?,
        audio.context("corrected period has no AAC")?,
    ))
}

fn reserve_samples(reader: &Reader<'_>, remaining: &mut u32) -> anyhow::Result<()> {
    for track in reader.tracks().values() {
        *remaining = remaining
            .checked_sub(track.sample_count())
            .context("corrected playback sample limit exceeded")?;
    }
    Ok(())
}

fn observe(
    reader: &mut Reader<'_>,
    video_track: &Track,
    audio_track: &Track,
    period: Period<'_>,
    options: Options,
    work: &Work<'_>,
) -> anyhow::Result<AudioTimingEvidence> {
    let first_video_pts_us = first_pts(reader, video_track, period, options, work)?;
    let first_audio_pts_us = first_pts(reader, audio_track, period, options, work)?;
    Ok(AudioTimingEvidence {
        first_video_pts_us,
        first_audio_pts_us,
        audio_present: first_audio_pts_us.is_some(),
        clipped_audio_samples: 0,
    })
}

fn first_pts(
    reader: &mut Reader<'_>,
    track: &Track,
    period: Period<'_>,
    options: Options,
    work: &Work<'_>,
) -> anyhow::Result<Option<i64>> {
    let mut earliest = None;
    for index in 1..=reader.sample_count(track.id)? {
        work.check()?;
        let sample = reader
            .read_sample(track.id, index)?
            .context("missing observed sample")?;
        let ticks = normalized_start(&sample, track.config.timescale, period, options)?
            .checked_add(i128::from(sample.rendering_offset))
            .context("observed PTS overflow")?;
        let micros = ticks
            .checked_mul(1_000_000)
            .context("observed microseconds overflow")?
            .div_euclid(i128::from(track.config.timescale));
        let micros = i64::try_from(micros).context("observed PTS exceeds microsecond clock")?;
        earliest = Some(earliest.map_or(micros, |previous: i64| previous.min(micros)));
    }
    Ok(earliest)
}

fn normalized_start(
    sample: &mp4::Mp4Sample,
    timescale: u32,
    period: Period<'_>,
    options: Options,
) -> anyhow::Result<i128> {
    let origin = (i128::from(period.recording_started_at_ms)
        - i128::from(options.playback_origin_ms))
    .checked_mul(i128::from(timescale))
    .context("recording origin overflow")?
    .div_euclid(1_000);
    i128::from(sample.start_time)
        .checked_add(origin)
        .context("normalized decode time overflow")
}

fn video_samples(
    reader: &mut Reader<'_>,
    track: &Track,
    period: Period<'_>,
    options: Options,
    work: &Work<'_>,
) -> anyhow::Result<Vec<mp4::Mp4Sample>> {
    let count = reader.sample_count(track.id)?;
    ensure!(count > 0, "corrected fragment has no video samples");
    let mut samples = Vec::with_capacity(usize::try_from(count)?);
    for index in 1..=count {
        work.check()?;
        ensure!(
            reader.tracks()[&track.id].sample_description_index(index)? == track.description,
            "video description changes inside corrected fragment"
        );
        let mut sample = reader
            .read_sample(track.id, index)?
            .context("missing corrected video sample")?;
        sample.start_time = u64::try_from(normalized_start(
            &sample,
            track.config.timescale,
            period,
            options,
        )?)
        .context("video precedes cursor origin")?;
        validate_sample_end(&sample)?;
        samples.push(sample);
    }
    validate_durations(&samples)?;
    Ok(samples)
}

fn audio_samples(
    context: &[Period<'_>],
    expected: &Track,
    options: Options,
    remaining_samples: &mut u32,
    evidence: &mut AudioTimingEvidence,
    work: &Work<'_>,
) -> anyhow::Result<Vec<mp4::Mp4Sample>> {
    let mut packets = BTreeMap::new();
    for &period in context {
        work.check()?;
        let mut reader = parse(period, work)?;
        reserve_samples(&reader, remaining_samples)?;
        let (_, track) = tracks(&reader)?;
        ensure!(
            track.config == expected.config,
            "neighbor audio codec or timescale changed"
        );
        collect_audio(&mut reader, &track, period, options, &mut packets, work)?;
    }
    let mut selected = Vec::new();
    for (start, mut sample) in packets {
        work.check()?;
        let Some(corrected) = audio_timing::audio_start(
            start,
            sample.rendering_offset,
            options.audio_offset_ms,
            expected.config.timescale,
        )?
        else {
            evidence.clipped_audio_samples = evidence
                .clipped_audio_samples
                .checked_add(1)
                .context("clipped sample count overflow")?;
            continue;
        };
        sample.start_time = corrected;
        validate_sample_end(&sample)?;
        if presentation_in_window(&sample, expected.config.timescale, options)? {
            selected.push(sample);
        }
    }
    // ponytail: Reject discontinuous audio runs; this writer would stretch packets over gaps.
    validate_durations(&selected)?;
    Ok(selected)
}

fn collect_audio(
    reader: &mut Reader<'_>,
    track: &Track,
    period: Period<'_>,
    options: Options,
    packets: &mut BTreeMap<i128, mp4::Mp4Sample>,
    work: &Work<'_>,
) -> anyhow::Result<()> {
    for index in 1..=reader.sample_count(track.id)? {
        work.check()?;
        ensure!(
            reader.tracks()[&track.id].sample_description_index(index)? == track.description,
            "audio description changes inside corrected fragment"
        );
        let sample = reader
            .read_sample(track.id, index)?
            .context("missing neighbor audio sample")?;
        ensure!(
            options.audio_offset_ms == 0 || sample.rendering_offset == 0,
            "corrected stored audio does not support AAC composition offsets"
        );
        let start = normalized_start(&sample, track.config.timescale, period, options)?;
        if let Some(previous) = packets.get(&start) {
            ensure!(
                previous.duration == sample.duration
                    && previous.rendering_offset == sample.rendering_offset
                    && previous.is_sync == sample.is_sync
                    && previous.bytes == sample.bytes,
                "conflicting overlapping audio periods"
            );
        } else {
            packets.insert(start, sample);
        }
    }
    Ok(())
}

fn presentation_in_window(
    sample: &mp4::Mp4Sample,
    timescale: u32,
    options: Options,
) -> anyhow::Result<bool> {
    let pts = i128::from(sample.start_time)
        .checked_add(i128::from(sample.rendering_offset))
        .and_then(|ticks| ticks.checked_mul(1_000))
        .context("corrected window comparison overflow")?;
    let start = (i128::from(options.window_start_ms) - i128::from(options.playback_origin_ms))
        .checked_mul(i128::from(timescale))
        .context("corrected window start overflow")?;
    let end = (i128::from(options.window_end_ms) - i128::from(options.playback_origin_ms))
        .checked_mul(i128::from(timescale))
        .context("corrected window end overflow")?;
    Ok(pts >= start && pts < end)
}

fn validate_sample_end(sample: &mp4::Mp4Sample) -> anyhow::Result<()> {
    ensure!(
        sample.duration > 0 && !sample.bytes.is_empty(),
        "corrected sample is empty"
    );
    let end = sample
        .start_time
        .checked_add(u64::from(sample.duration))
        .context("corrected sample end overflow")?;
    let presentation_end = i128::from(end)
        .checked_add(i128::from(sample.rendering_offset))
        .context("corrected presentation end overflow")?;
    u64::try_from(presentation_end).context("corrected presentation end exceeds track clock")?;
    Ok(())
}

fn validate_durations(samples: &[mp4::Mp4Sample]) -> anyhow::Result<()> {
    for pair in samples.windows(2) {
        ensure!(
            pair[0].start_time.checked_add(u64::from(pair[0].duration)) == Some(pair[1].start_time),
            "corrected sample run has a gap, overlap, or clipped interior packet"
        );
    }
    Ok(())
}

fn write_samples(
    writer: &mut mp4::FragmentedMp4Writer<BoundedBytes<'_>>,
    track_id: u32,
    samples: Vec<mp4::Mp4Sample>,
    work: &Work<'_>,
) -> anyhow::Result<()> {
    for sample in samples {
        work.check()?;
        writer.write_sample(track_id, sample)?;
    }
    Ok(())
}

impl<'a> BoundedBytes<'a> {
    fn new(bytes: Vec<u8>, work: &Work<'a>) -> Self {
        Self {
            bytes: Cursor::new(bytes),
            work: Work {
                deadline: work.deadline,
                cancelled: work.cancelled,
            },
        }
    }
}

impl Read for BoundedBytes<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.work.check().map_err(io::Error::other)?;
        self.bytes.read(bytes)
    }
}

impl Seek for BoundedBytes<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.work.check().map_err(io::Error::other)?;
        let offset = self.bytes.seek(position)?;
        if offset > OUTPUT_BYTES_MAX {
            return Err(io::Error::other("corrected output seek limit exceeded"));
        }
        Ok(offset)
    }
}

impl Write for BoundedBytes<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.work.check().map_err(io::Error::other)?;
        let end = self
            .bytes
            .position()
            .checked_add(u64::try_from(bytes.len()).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("corrected output size overflow"))?;
        if end > OUTPUT_BYTES_MAX {
            return Err(io::Error::other("corrected output byte limit exceeded"));
        }
        self.bytes.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.work.check().map_err(io::Error::other)?;
        self.bytes.flush()
    }
}

#[cfg(test)]
mod tests;
