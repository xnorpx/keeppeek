use anyhow::{Context, bail};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
};

use crate::storage::CatalogMediaFragment;

#[derive(Clone)]
struct PlaybackTrack {
    source_id: u32,
    config: mp4::TrackConfig,
    sample_descriptions: Vec<mp4::MediaConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportArtifact {
    pub aligned_start_ms: i64,
    pub delivered_end_ms: i64,
    pub bytes: u64,
}

struct ExportRecording {
    started_at_ms: i64,
    path: String,
    intervals: Vec<(i64, i64)>,
}

struct ExportSample {
    absolute_ms: i64,
    track_id: u32,
    track_type: mp4::TrackType,
    sample_description: mp4::MediaConfig,
    sample: mp4::Mp4Sample,
}

#[derive(Default)]
struct ExportProgress {
    pending_video: bool,
    last_sample_start: HashMap<u32, u64>,
    bytes_written: u64,
}

struct ExportCallbacks<'a> {
    cancelled: &'a dyn Fn() -> bool,
    report_progress: &'a dyn Fn(u64),
}

pub fn export_fragment_ranges(
    fragments: &[CatalogMediaFragment],
    requested_end_ms: i64,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<ExportArtifact> {
    export_fragment_ranges_with_progress(
        fragments,
        requested_end_ms,
        destination,
        cancelled,
        |_| {},
    )
}

pub fn export_fragment_ranges_with_progress(
    fragments: &[CatalogMediaFragment],
    requested_end_ms: i64,
    destination: &Path,
    cancelled: impl Fn() -> bool,
    progress: impl Fn(u64),
) -> anyhow::Result<ExportArtifact> {
    let aligned_start_ms = fragments
        .first()
        .context("export range has no recorded fragments")?
        .start_ms;
    let delivered_end_ms = fragments
        .iter()
        .map(|fragment| {
            fragment
                .start_ms
                .saturating_add(i64::try_from(fragment.duration_ms).unwrap_or(i64::MAX))
        })
        .max()
        .unwrap_or(aligned_start_ms)
        .min(requested_end_ms);
    let mut recordings = HashMap::<String, ExportRecording>::new();
    for fragment in fragments {
        let end_ms = fragment
            .start_ms
            .saturating_add(i64::try_from(fragment.duration_ms).unwrap_or(i64::MAX))
            .min(requested_end_ms);
        let recording = recordings
            .entry(fragment.recording_id.clone())
            .or_insert_with(|| ExportRecording {
                started_at_ms: fragment.recording_started_at_ms,
                path: fragment.path.clone(),
                intervals: Vec::new(),
            });
        recording.intervals.push((fragment.start_ms, end_ms));
    }
    let mut recordings = recordings.into_values().collect::<Vec<_>>();
    recordings.sort_unstable_by_key(|recording| recording.started_at_ms);

    let temporary =
        destination.with_extension(format!("mp4.{}.active", uuid::Uuid::new_v4().hyphenated()));
    if let Some(parent) = temporary.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result = write_export_recordings(
        &recordings,
        aligned_start_ms,
        &temporary,
        &cancelled,
        &progress,
    )
    .and_then(|()| {
        if cancelled() {
            bail!("export was cancelled");
        }
        std::fs::rename(&temporary, destination)?;
        Ok(ExportArtifact {
            aligned_start_ms,
            delivered_end_ms,
            bytes: destination.metadata()?.len(),
        })
    });
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn write_export_recordings(
    recordings: &[ExportRecording],
    aligned_start_ms: i64,
    destination: &Path,
    cancelled: &impl Fn() -> bool,
    report_progress: &impl Fn(u64),
) -> anyhow::Result<()> {
    let first = recordings
        .first()
        .context("export has no source recordings")?;
    let mut first_reader = mp4::read_mp4(File::open(&first.path)?)?;
    let reference_tracks = export_tracks(&first_reader)?;
    let selected_track_indexes =
        selected_export_track_indexes(recordings, &reference_tracks, aligned_start_ms, cancelled)?;
    let tracks = selected_track_indexes
        .iter()
        .map(|index| reference_tracks[*index].clone())
        .collect::<Vec<_>>();
    let configs = tracks
        .iter()
        .map(|track| mp4::FragmentedTrackConfig {
            track_type: track.config.track_type,
            timescale: track.config.timescale,
            language: track.config.language.clone(),
            sample_descriptions: track.sample_descriptions.clone(),
        })
        .collect::<Vec<_>>();
    if !configs
        .iter()
        .any(|config| config.track_type == mp4::TrackType::Video)
    {
        bail!("export source has no supported video track");
    }
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec![
            "iso6".parse().unwrap(),
            "isom".parse().unwrap(),
            "mp41".parse().unwrap(),
        ],
        timescale: 1_000,
    };
    let output = BufWriter::new(File::create(destination)?);
    let mut writer =
        mp4::FragmentedMp4Writer::write_start_with_sample_descriptions(output, &config, &configs)?;
    let mut progress = ExportProgress::default();
    let callbacks = ExportCallbacks {
        cancelled,
        report_progress,
    };

    write_export_recording(
        &mut first_reader,
        first,
        &tracks,
        aligned_start_ms,
        &mut writer,
        &mut progress,
        &callbacks,
    )?;
    for recording in recordings.iter().skip(1) {
        let mut reader = mp4::read_mp4(File::open(&recording.path)?)?;
        let source_tracks = export_tracks(&reader)?;
        validate_track_layout(&reference_tracks, &source_tracks)?;
        let selected_tracks = selected_track_indexes
            .iter()
            .map(|index| source_tracks[*index].clone())
            .collect::<Vec<_>>();
        write_export_recording(
            &mut reader,
            recording,
            &selected_tracks,
            aligned_start_ms,
            &mut writer,
            &mut progress,
            &callbacks,
        )?;
    }
    writer.write_end()?;
    let mut output = writer.into_writer();
    output.flush()?;
    Ok(())
}

fn selected_export_track_indexes(
    recordings: &[ExportRecording],
    reference_tracks: &[PlaybackTrack],
    aligned_start_ms: i64,
    cancelled: &impl Fn() -> bool,
) -> anyhow::Result<Vec<usize>> {
    let mut selected = vec![false; reference_tracks.len()];
    for recording in recordings {
        let mut reader = mp4::read_mp4(File::open(&recording.path)?)?;
        let tracks = export_tracks(&reader)?;
        validate_track_layout(reference_tracks, &tracks)?;
        for (index, track) in tracks.iter().enumerate() {
            if !selected[index]
                && track_has_selected_sample(
                    &mut reader,
                    recording,
                    track,
                    aligned_start_ms,
                    cancelled,
                )?
            {
                selected[index] = true;
            }
        }
    }
    Ok(selected
        .into_iter()
        .enumerate()
        .filter_map(|(index, selected)| selected.then_some(index))
        .collect())
}

fn validate_track_layout(
    reference: &[PlaybackTrack],
    candidate: &[PlaybackTrack],
) -> anyhow::Result<()> {
    if candidate.len() != reference.len()
        || candidate
            .iter()
            .zip(reference)
            .any(|(candidate, reference)| {
                candidate.config.track_type != reference.config.track_type
                    || candidate.config.timescale != reference.config.timescale
                    || candidate.config.language != reference.config.language
            })
    {
        bail!("export crosses an incompatible track layout change");
    }
    Ok(())
}

fn track_has_selected_sample<R: Read + Seek>(
    reader: &mut mp4::Mp4Reader<R>,
    recording: &ExportRecording,
    track: &PlaybackTrack,
    aligned_start_ms: i64,
    cancelled: &impl Fn() -> bool,
) -> anyhow::Result<bool> {
    for sample_id in 1..=reader.sample_count(track.source_id)? {
        if cancelled() {
            bail!("export was cancelled");
        }
        let sample = reader
            .read_sample(track.source_id, sample_id)?
            .with_context(|| format!("track {} sample {sample_id} is missing", track.source_id))?;
        let start_offset_ms =
            sample.start_time.saturating_mul(1_000) / u64::from(track.config.timescale);
        let end_offset_ms = sample
            .start_time
            .saturating_add(u64::from(sample.duration))
            .saturating_mul(1_000)
            / u64::from(track.config.timescale);
        let absolute_ms = recording
            .started_at_ms
            .saturating_add(i64::try_from(start_offset_ms).unwrap_or(i64::MAX));
        let absolute_end_ms = recording
            .started_at_ms
            .saturating_add(i64::try_from(end_offset_ms).unwrap_or(i64::MAX));
        let overlaps = recording
            .intervals
            .iter()
            .any(|(start, end)| absolute_ms < *end && absolute_end_ms > *start);
        if !overlaps {
            continue;
        }
        let origin_ticks = i128::from(recording.started_at_ms - aligned_start_ms)
            * i128::from(track.config.timescale)
            / 1_000;
        if origin_ticks + i128::from(sample.start_time) >= 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn export_tracks<R: Read + Seek>(reader: &mp4::Mp4Reader<R>) -> anyhow::Result<Vec<PlaybackTrack>> {
    let mut tracks = reader
        .tracks()
        .iter()
        .filter_map(|(&source_id, track)| match playback_track(track) {
            Ok(Some(mut playback)) => {
                playback.source_id = source_id;
                Some(Ok(playback))
            }
            Ok(None) => None,
            Err(error) if track.track_type().ok() == Some(mp4::TrackType::Audio) => {
                tracing::warn!(track_id = source_id, %error, "omitting invalid audio track from export");
                None
            }
            Err(error) => Some(Err(error)),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    tracks.sort_unstable_by_key(|track| track.source_id);
    Ok(tracks)
}

fn write_export_recording<R: Read + Seek>(
    reader: &mut mp4::Mp4Reader<R>,
    recording: &ExportRecording,
    tracks: &[PlaybackTrack],
    aligned_start_ms: i64,
    writer: &mut mp4::FragmentedMp4Writer<BufWriter<File>>,
    progress: &mut ExportProgress,
    callbacks: &ExportCallbacks<'_>,
) -> anyhow::Result<()> {
    let mut samples = Vec::new();
    for (index, track) in tracks.iter().enumerate() {
        let sample_count = reader.sample_count(track.source_id)?;
        for sample_id in 1..=sample_count {
            if (callbacks.cancelled)() {
                bail!("export was cancelled");
            }
            let mut sample = reader
                .read_sample(track.source_id, sample_id)?
                .with_context(|| {
                    format!("track {} sample {sample_id} is missing", track.source_id)
                })?;
            let sample_description_index = reader
                .tracks()
                .get(&track.source_id)
                .context("export source track disappeared")?
                .sample_description_index(sample_id)?;
            let sample_description = reader
                .tracks()
                .get(&track.source_id)
                .context("export source track disappeared")?
                .media_config_for_description(sample_description_index)?;
            let start_offset_ms =
                sample.start_time.saturating_mul(1_000) / u64::from(track.config.timescale);
            let end_offset_ms = sample
                .start_time
                .saturating_add(u64::from(sample.duration))
                .saturating_mul(1_000)
                / u64::from(track.config.timescale);
            let absolute_ms = recording
                .started_at_ms
                .saturating_add(i64::try_from(start_offset_ms).unwrap_or(i64::MAX));
            let absolute_end_ms = recording
                .started_at_ms
                .saturating_add(i64::try_from(end_offset_ms).unwrap_or(i64::MAX));
            if !recording
                .intervals
                .iter()
                .any(|(start, end)| absolute_ms < *end && absolute_end_ms > *start)
            {
                continue;
            }
            let origin_ticks = i128::from(recording.started_at_ms - aligned_start_ms)
                * i128::from(track.config.timescale)
                / 1_000;
            let normalized_start = origin_ticks + i128::from(sample.start_time);
            let Ok(normalized_start) = u64::try_from(normalized_start) else {
                if track.config.track_type != mp4::TrackType::Video {
                    continue;
                }
                bail!("export video sample timestamp precedes aligned start");
            };
            sample.start_time = normalized_start;
            samples.push(ExportSample {
                absolute_ms,
                track_id: u32::try_from(index + 1).unwrap_or(u32::MAX),
                track_type: track.config.track_type,
                sample_description,
                sample,
            });
        }
    }
    samples.sort_unstable_by(|left, right| {
        left.absolute_ms.cmp(&right.absolute_ms).then_with(|| {
            let left_rank = u8::from(left.track_type != mp4::TrackType::Video);
            let right_rank = u8::from(right.track_type != mp4::TrackType::Video);
            left_rank.cmp(&right_rank)
        })
    });
    for sample in samples {
        if (callbacks.cancelled)() {
            bail!("export was cancelled");
        }
        if progress
            .last_sample_start
            .get(&sample.track_id)
            .is_some_and(|previous| sample.sample.start_time <= *previous)
        {
            continue;
        }
        if sample.track_type == mp4::TrackType::Video && sample.sample.is_sync {
            if progress.pending_video {
                writer.flush_fragment()?;
            }
            progress.pending_video = true;
        }
        let description_index =
            writer.add_sample_description(sample.track_id, sample.sample_description)?;
        progress
            .last_sample_start
            .insert(sample.track_id, sample.sample.start_time);
        let sample_bytes = u64::try_from(sample.sample.bytes.len()).unwrap_or(u64::MAX);
        writer.write_sample_with_description(sample.track_id, description_index, sample.sample)?;
        progress.bytes_written = progress.bytes_written.saturating_add(sample_bytes);
        (callbacks.report_progress)(progress.bytes_written);
    }
    Ok(())
}

pub fn browser_compatible_recording(source: &Path) -> anyhow::Result<PathBuf> {
    let cached = source.with_extension("mp4.browser");
    if cached.is_file() {
        return Ok(cached);
    }

    let temporary = source.with_extension(format!("mp4.browser.{}.active", rand::random::<u64>()));
    let result = remux_recording(source, &temporary).and_then(|()| {
        if cached.exists() {
            std::fs::remove_file(&temporary)?;
        } else {
            std::fs::rename(&temporary, &cached)?;
        }
        Ok(cached.clone())
    });
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn remux_recording(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let mut reader = mp4::read_mp4(File::open(source)?)?;
    let mut tracks = reader
        .tracks()
        .iter()
        .filter_map(|(&source_id, track)| match playback_track(track) {
            Ok(Some(mut playback)) => {
                playback.source_id = source_id;
                Some(Ok(playback))
            }
            Ok(None) => None,
            Err(error) if track.track_type().ok() == Some(mp4::TrackType::Audio) => {
                tracing::warn!(path = %source.display(), track_id = source_id, %error, "omitting invalid audio track from browser compatibility recording");
                None
            }
            Err(error) => Some(Err(error)),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    tracks.sort_unstable_by_key(|track| track.source_id);
    if !tracks
        .iter()
        .any(|track| track.config.track_type == mp4::TrackType::Video)
    {
        bail!("recording has no supported video track");
    }

    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec![
            "iso6".parse().unwrap(),
            "isom".parse().unwrap(),
            "mp41".parse().unwrap(),
        ],
        timescale: 1_000,
    };
    let output = BufWriter::new(File::create(destination)?);
    let track_configs = tracks
        .iter()
        .map(|track| mp4::FragmentedTrackConfig {
            track_type: track.config.track_type,
            timescale: track.config.timescale,
            language: track.config.language.clone(),
            sample_descriptions: track.sample_descriptions.clone(),
        })
        .collect::<Vec<_>>();
    let mut writer = mp4::FragmentedMp4Writer::write_start_with_sample_descriptions(
        output,
        &config,
        &track_configs,
    )?;
    let mut samples = Vec::new();
    for (output_index, track) in tracks.iter().enumerate() {
        let sample_count = reader.sample_count(track.source_id)?;
        for sample_id in 1..=sample_count {
            let sample = reader
                .read_sample(track.source_id, sample_id)?
                .with_context(|| {
                    format!("track {} sample {sample_id} is missing", track.source_id)
                })?;
            let description_index =
                reader.tracks()[&track.source_id].sample_description_index(sample_id)?;
            let sample_description = reader.tracks()[&track.source_id]
                .media_config_for_description(description_index)?;
            samples.push(ExportSample {
                absolute_ms: i64::try_from(
                    sample.start_time.saturating_mul(1_000) / u64::from(track.config.timescale),
                )
                .unwrap_or(i64::MAX),
                track_id: u32::try_from(output_index + 1).unwrap_or(u32::MAX),
                track_type: track.config.track_type,
                sample_description,
                sample,
            });
        }
    }
    samples.sort_unstable_by(|left, right| {
        left.absolute_ms.cmp(&right.absolute_ms).then_with(|| {
            let left_rank = u8::from(left.track_type != mp4::TrackType::Video);
            let right_rank = u8::from(right.track_type != mp4::TrackType::Video);
            left_rank.cmp(&right_rank)
        })
    });
    let mut pending_video = false;
    for sample in samples {
        if sample.track_type == mp4::TrackType::Video && sample.sample.is_sync {
            if pending_video {
                writer.flush_fragment()?;
            }
            pending_video = true;
        }
        let description_index =
            writer.add_sample_description(sample.track_id, sample.sample_description)?;
        writer.write_sample_with_description(sample.track_id, description_index, sample.sample)?;
    }
    writer.write_end()?;
    let mut output = writer.into_writer();
    output.flush()?;
    Ok(())
}

fn playback_track_config(track: &mp4::Mp4Track) -> anyhow::Result<Option<mp4::TrackConfig>> {
    let media_conf = match track.media_config_for_description(1)? {
        mp4::MediaConfig::AvcConfig(config) => mp4::MediaConfig::AvcConfig(config),
        mp4::MediaConfig::HevcConfig(config) => mp4::MediaConfig::HevcConfig(config),
        mp4::MediaConfig::AacConfig(mut config) => {
            config.bitrate = config.bitrate.max(64_000);
            return Ok(Some(mp4::TrackConfig {
                track_type: mp4::TrackType::Audio,
                timescale: config.freq_index.freq(),
                language: track.language().to_owned(),
                media_conf: mp4::MediaConfig::AacConfig(config),
            }));
        }
        mp4::MediaConfig::Vp9Config(_) | mp4::MediaConfig::TtxtConfig(_) => return Ok(None),
    };
    Ok(Some(mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: track.timescale(),
        language: track.language().to_owned(),
        media_conf,
    }))
}

fn playback_track(track: &mp4::Mp4Track) -> anyhow::Result<Option<PlaybackTrack>> {
    let Some(config) = playback_track_config(track)? else {
        return Ok(None);
    };
    let sample_descriptions = if config.track_type == mp4::TrackType::Video {
        (1..=track.sample_description_count())
            .map(|index| {
                track.media_config_for_description(u32::try_from(index).unwrap_or(u32::MAX))
            })
            .collect::<mp4::Result<Vec<_>>>()?
    } else {
        vec![config.media_conf.clone()]
    };
    Ok(Some(PlaybackTrack {
        source_id: track.track_id(),
        config,
        sample_descriptions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::storage::{
        AudioCodec, AudioFrame, MediaFrame, RecordingCatalog, RecordingFrame, VideoCodec,
        VideoFrame, medium_term::MediumTermWriter,
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    #[test]
    fn compatibility_remux_repairs_audio_timescale_and_is_cached() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-playback-remux-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("recording.mp4");
        write_malformed_recording(&source);

        let cached = browser_compatible_recording(&source).unwrap();
        let size = cached.metadata().unwrap().len();
        assert_eq!(browser_compatible_recording(&source).unwrap(), cached);
        assert_eq!(cached.metadata().unwrap().len(), size);

        let reader = mp4::read_mp4(File::open(&cached).unwrap()).unwrap();
        let video = reader
            .tracks()
            .values()
            .find(|track| track.track_type().unwrap() == mp4::TrackType::Video)
            .unwrap();
        let audio = reader
            .tracks()
            .values()
            .find(|track| track.track_type().unwrap() == mp4::TrackType::Audio)
            .unwrap();
        assert_eq!(video.duration(), std::time::Duration::from_secs(1));
        assert_eq!(audio.timescale(), 16_000);
        assert_eq!(audio.duration(), std::time::Duration::from_millis(64));
        assert_eq!(reader.duration(), std::time::Duration::from_secs(1));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_preserves_timestamp_gap_between_indexed_recordings() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-export-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&directory).unwrap();
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let now = Instant::now();
        write_export_source(&directory, now - Duration::from_secs(10), handle.clone());
        write_export_source(&directory, now - Duration::from_secs(5), handle.clone());
        let fragments = handle
            .media_fragments_in_range("front-door/main", i64::MIN + 1, i64::MAX)
            .unwrap();
        assert_eq!(fragments.len(), 4);
        let end_ms = fragments
            .last()
            .map(|fragment| {
                fragment
                    .start_ms
                    .saturating_add(i64::try_from(fragment.duration_ms).unwrap())
            })
            .unwrap();
        let destination = directory.join("export.mp4");

        let artifact = export_fragment_ranges(&fragments, end_ms, &destination, || false).unwrap();

        assert_eq!(artifact.aligned_start_ms, fragments[0].start_ms);
        assert_eq!(artifact.delivered_end_ms, end_ms);
        assert_eq!(artifact.bytes, destination.metadata().unwrap().len());
        let mut reader = mp4::read_mp4(File::open(&destination).unwrap()).unwrap();
        let (&track_id, track) = reader
            .tracks()
            .iter()
            .find(|(_, track)| track.track_type().unwrap() == mp4::TrackType::Video)
            .unwrap();
        let timescale = track.timescale();
        let count = reader.sample_count(track_id).unwrap();
        let start_times = (1..=count)
            .map(|sample_id| {
                reader
                    .read_sample(track_id, sample_id)
                    .unwrap()
                    .unwrap()
                    .start_time
                    * 1_000
                    / u64::from(timescale)
            })
            .collect::<Vec<_>>();
        assert_eq!(start_times.len(), 4);
        assert!(start_times[2].saturating_sub(start_times[1]) >= 3_000);

        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_preserves_mixed_codec_gop_descriptions() {
        fn fixture(name: &str, media_type: mp4::MediaType) -> (mp4::MediaConfig, mp4::Mp4Sample) {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/test-camera/testdata")
                .join(name);
            let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
            let (&track_id, track) = reader
                .tracks()
                .iter()
                .find(|(_, track)| track.media_type().ok() == Some(media_type))
                .unwrap();
            let config = track.media_config_for_description(1).unwrap();
            let mut sample = (1..=track.sample_count())
                .find_map(|sample_id| {
                    let sample = reader.read_sample(track_id, sample_id).unwrap().unwrap();
                    sample.is_sync.then_some(sample)
                })
                .unwrap();
            sample.start_time = 0;
            sample.duration = 90_000;
            (config, sample)
        }

        let directory = std::env::temp_dir().join(format!(
            "keeppeek-export-mixed-codec-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.mp4");
        let (h264, h264_sample) = fixture("cc-4k-640x360-h264.mp4", mp4::MediaType::H264);
        let (h265, mut h265_sample) = fixture("cc-4k-640x360-h265.mp4", mp4::MediaType::H265);
        h265_sample.start_time = 90_000;
        let config = mp4::Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
            timescale: 1_000,
        };
        let track = mp4::FragmentedTrackConfig {
            track_type: mp4::TrackType::Video,
            timescale: 90_000,
            language: "und".to_owned(),
            sample_descriptions: vec![h264, h265],
        };
        let mut writer = mp4::FragmentedMp4Writer::write_start_with_sample_descriptions(
            File::create(&source).unwrap(),
            &config,
            &[track],
        )
        .unwrap();
        let initialization = writer.initialization();
        writer
            .write_sample_with_description(1, 1, h264_sample)
            .unwrap();
        let first = writer.flush_fragment().unwrap().unwrap();
        writer
            .write_sample_with_description(1, 2, h265_sample)
            .unwrap();
        let second = writer.write_end().unwrap().unwrap();
        drop(writer);
        let fragments = [
            CatalogMediaFragment {
                recording_id: "mixed".to_owned(),
                recording_started_at_ms: 0,
                path: source.to_string_lossy().into_owned(),
                init_offset: initialization.offset,
                init_len: initialization.size,
                sequence: 1,
                start_ms: 0,
                duration_ms: 1_000,
                byte_offset: first.range.offset,
                byte_len: first.range.size,
            },
            CatalogMediaFragment {
                recording_id: "mixed".to_owned(),
                recording_started_at_ms: 0,
                path: source.to_string_lossy().into_owned(),
                init_offset: initialization.offset,
                init_len: initialization.size,
                sequence: 2,
                start_ms: 1_000,
                duration_ms: 1_000,
                byte_offset: second.range.offset,
                byte_len: second.range.size,
            },
        ];
        let destination = directory.join("export.mp4");
        export_fragment_ranges(&fragments, 2_000, &destination, || false).unwrap();

        let reader = mp4::read_mp4(File::open(destination).unwrap()).unwrap();
        let track = reader
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
            .unwrap();
        assert_eq!(track.sample_description_count(), 2);
        assert_eq!(track.sample_description_index(1).unwrap(), 1);
        assert_eq!(track.sample_description_index(2).unwrap(), 2);
        assert_eq!(
            track.media_type_for_description(1).unwrap(),
            mp4::MediaType::H264
        );
        assert_eq!(
            track.media_type_for_description(2).unwrap(),
            mp4::MediaType::H265
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_registers_codec_changes_across_recording_files() {
        fn fixture(name: &str, media_type: mp4::MediaType) -> (mp4::MediaConfig, mp4::Mp4Sample) {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/test-camera/testdata")
                .join(name);
            let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
            let (&track_id, track) = reader
                .tracks()
                .iter()
                .find(|(_, track)| track.media_type().ok() == Some(media_type))
                .unwrap();
            let config = track.media_config_for_description(1).unwrap();
            let mut sample = (1..=track.sample_count())
                .find_map(|sample_id| {
                    let sample = reader.read_sample(track_id, sample_id).unwrap().unwrap();
                    sample.is_sync.then_some(sample)
                })
                .unwrap();
            sample.start_time = 0;
            sample.duration = 90_000;
            (config, sample)
        }

        fn write_source(
            path: &Path,
            config: mp4::MediaConfig,
            sample: mp4::Mp4Sample,
        ) -> (mp4::Mp4ByteRange, mp4::Mp4FragmentInfo) {
            let track = mp4::TrackConfig {
                track_type: mp4::TrackType::Video,
                timescale: 90_000,
                language: "und".to_owned(),
                media_conf: config,
            };
            let mut writer = mp4::FragmentedMp4Writer::write_start(
                File::create(path).unwrap(),
                &mp4::Mp4Config {
                    major_brand: "iso6".parse().unwrap(),
                    minor_version: 1,
                    compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
                    timescale: 1_000,
                },
                &[track],
            )
            .unwrap();
            let initialization = writer.initialization();
            writer.write_sample(1, sample).unwrap();
            let fragment = writer.write_end().unwrap().unwrap();
            (initialization, fragment)
        }

        let directory = std::env::temp_dir().join(format!(
            "keeppeek-export-cross-recording-codec-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let h264_path = directory.join("h264.mp4");
        let h265_path = directory.join("h265.mp4");
        let (h264_config, h264_sample) = fixture("cc-4k-640x360-h264.mp4", mp4::MediaType::H264);
        let (h265_config, h265_sample) = fixture("cc-4k-640x360-h265.mp4", mp4::MediaType::H265);
        let (h264_init, h264_fragment) = write_source(&h264_path, h264_config, h264_sample);
        let (h265_init, h265_fragment) = write_source(&h265_path, h265_config, h265_sample);
        let fragments = [
            CatalogMediaFragment {
                recording_id: "h264".to_owned(),
                recording_started_at_ms: 0,
                path: h264_path.to_string_lossy().into_owned(),
                init_offset: h264_init.offset,
                init_len: h264_init.size,
                sequence: 1,
                start_ms: 0,
                duration_ms: 1_000,
                byte_offset: h264_fragment.range.offset,
                byte_len: h264_fragment.range.size,
            },
            CatalogMediaFragment {
                recording_id: "h265".to_owned(),
                recording_started_at_ms: 1_000,
                path: h265_path.to_string_lossy().into_owned(),
                init_offset: h265_init.offset,
                init_len: h265_init.size,
                sequence: 1,
                start_ms: 1_000,
                duration_ms: 1_000,
                byte_offset: h265_fragment.range.offset,
                byte_len: h265_fragment.range.size,
            },
        ];
        let destination = directory.join("cross-recording-export.mp4");
        export_fragment_ranges(&fragments, 2_000, &destination, || false).unwrap();
        let reader = mp4::read_mp4(File::open(destination).unwrap()).unwrap();
        let video = reader
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
            .unwrap();
        assert_eq!(video.sample_description_count(), 2);
        assert_eq!(video.sample_description_index(1).unwrap(), 1);
        assert_eq!(video.sample_description_index(2).unwrap(), 2);
        assert_eq!(
            video.media_type_for_description(1).unwrap(),
            mp4::MediaType::H264
        );
        assert_eq!(
            video.media_type_for_description(2).unwrap(),
            mp4::MediaType::H265
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cancelled_export_removes_partial_file_and_can_retry() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-export-cancel-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&directory).unwrap();
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        write_export_source(&directory, Instant::now(), handle.clone());
        let fragments = handle
            .media_fragments_in_range("front-door/main", i64::MIN + 1, i64::MAX)
            .unwrap();
        let end_ms = fragments
            .last()
            .map(|fragment| {
                fragment
                    .start_ms
                    .saturating_add(i64::try_from(fragment.duration_ms).unwrap())
            })
            .unwrap();
        let destination = directory.join("cancelled.mp4");
        let checks = Arc::new(AtomicUsize::new(0));
        let error = export_fragment_ranges(&fragments, end_ms, &destination, move || {
            checks.fetch_add(1, Ordering::Relaxed) >= 1
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "export was cancelled");
        assert!(!destination.exists());
        assert!(std::fs::read_dir(&directory).unwrap().all(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("active")
        }));

        let artifact = export_fragment_ranges(&fragments, end_ms, &destination, || false).unwrap();
        assert!(destination.is_file());
        assert_eq!(artifact.bytes, destination.metadata().unwrap().len());

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_keeps_aac_timestamps_monotonic_across_recordings() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-export-audio-continuity-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let now = Instant::now();
        write_audio_export_source(&directory, now - Duration::from_secs(2), handle.clone());
        write_audio_export_source(&directory, now, handle.clone());
        let fragments = handle
            .media_fragments_in_range("front-door/main", i64::MIN + 1, i64::MAX)
            .unwrap();
        let end_ms = fragments
            .last()
            .map(|fragment| {
                fragment
                    .start_ms
                    .saturating_add(i64::try_from(fragment.duration_ms).unwrap())
            })
            .unwrap();
        let destination = directory.join("audio-export.mp4");
        export_fragment_ranges(&fragments, end_ms, &destination, || false).unwrap();

        let mut reader = mp4::read_mp4(File::open(destination).unwrap()).unwrap();
        let (&audio_track_id, audio_track) = reader
            .tracks()
            .iter()
            .find(|(_, track)| track.track_type().ok() == Some(mp4::TrackType::Audio))
            .unwrap();
        let samples = (1..=audio_track.sample_count())
            .map(|sample_id| {
                reader
                    .read_sample(audio_track_id, sample_id)
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(samples.len(), 6);
        assert!(samples.windows(2).all(|pair| {
            pair[0]
                .start_time
                .saturating_add(u64::from(pair[0].duration))
                <= pair[1].start_time
        }));

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_omits_overlapping_samples_across_recording_boundaries() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-export-overlapping-recordings-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let started_at = Instant::now();
        write_audio_export_source(&directory, started_at, handle.clone());
        write_audio_export_source(
            &directory,
            started_at + Duration::from_millis(50),
            handle.clone(),
        );
        let fragments = handle
            .media_fragments_in_range("front-door/main", i64::MIN + 1, i64::MAX)
            .unwrap();
        let end_ms = fragments
            .iter()
            .map(|fragment| {
                fragment
                    .start_ms
                    .saturating_add(i64::try_from(fragment.duration_ms).unwrap())
            })
            .max()
            .unwrap();
        let destination = directory.join("overlapping-export.mp4");

        export_fragment_ranges(&fragments, end_ms, &destination, || false).unwrap();

        let mut reader = mp4::read_mp4(File::open(destination).unwrap()).unwrap();
        let track_ids = reader.tracks().keys().copied().collect::<Vec<_>>();
        for track_id in track_ids {
            let sample_count = reader.tracks()[&track_id].sample_count();
            let samples = (1..=sample_count)
                .map(|sample_id| reader.read_sample(track_id, sample_id).unwrap().unwrap())
                .collect::<Vec<_>>();
            assert!(
                samples
                    .windows(2)
                    .all(|pair| pair[0].start_time < pair[1].start_time)
            );
        }

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn export_omits_audio_preroll_before_aligned_video_keyframe() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-export-audio-preroll-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        write_audio_export_source(&directory, Instant::now(), handle.clone());
        let fragments = handle
            .media_fragments_in_range("front-door/main", i64::MIN + 1, i64::MAX)
            .unwrap();
        assert_eq!(fragments.len(), 2);
        let selected = &fragments[1..];
        let end_ms = selected[0]
            .start_ms
            .saturating_add(i64::try_from(selected[0].duration_ms).unwrap());
        let destination = directory.join("audio-preroll-export.mp4");

        export_fragment_ranges(selected, end_ms, &destination, || false).unwrap();

        let reader = mp4::read_mp4(File::open(destination).unwrap()).unwrap();
        let video_samples = reader
            .tracks()
            .values()
            .find(|track| track.track_type().unwrap() == mp4::TrackType::Video)
            .unwrap()
            .sample_count();
        let audio_track = reader
            .tracks()
            .values()
            .find(|track| track.track_type().unwrap() == mp4::TrackType::Audio);
        assert_eq!(video_samples, 1);
        assert!(audio_track.is_none());

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn write_export_source(
        directory: &Path,
        started_at: Instant,
        catalog: crate::storage::RecordingCatalogHandle,
    ) {
        let mut writer = MediumTermWriter::create_with_catalog(
            directory,
            "front-door/main",
            started_at,
            8 * 1_024,
            catalog,
        )
        .unwrap();
        let payload = bytes::Bytes::from_static(&[
            0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
            0x3c, 0x80, 0, 0, 0, 1, 0x65,
        ]);
        for offset_ms in [0, 1_000] {
            writer
                .append_one(RecordingFrame {
                    received_at: started_at + Duration::from_millis(offset_ms),
                    timestamp: Some(Duration::from_millis(offset_ms)),
                    frame: MediaFrame::Video(VideoFrame {
                        codec: VideoCodec::H264,
                        is_keyframe: true,
                        width: 640,
                        height: 360,
                        data: payload.clone(),
                    }),
                })
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    fn write_audio_export_source(
        directory: &Path,
        started_at: Instant,
        catalog: crate::storage::RecordingCatalogHandle,
    ) {
        let mut writer = MediumTermWriter::create_with_catalog(
            directory,
            "front-door/main",
            started_at,
            8 * 1_024,
            catalog,
        )
        .unwrap();
        let payload = bytes::Bytes::from_static(&[
            0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
            0x3c, 0x80, 0, 0, 0, 1, 0x65,
        ]);
        writer
            .append_one(RecordingFrame {
                received_at: started_at,
                timestamp: Some(Duration::ZERO),
                frame: MediaFrame::Video(VideoFrame {
                    codec: VideoCodec::H264,
                    is_keyframe: true,
                    width: 640,
                    height: 360,
                    data: payload.clone(),
                }),
            })
            .unwrap();
        for offset_ms in [10, 30, 90] {
            writer
                .append_one(RecordingFrame {
                    received_at: started_at + Duration::from_millis(offset_ms),
                    timestamp: Some(Duration::from_millis(offset_ms)),
                    frame: MediaFrame::Audio(AudioFrame {
                        codec: AudioCodec::Aac,
                        sample_rate: 48_000,
                        duration: Duration::from_millis(20),
                        data: vec![0xff, 0xf1, 0x4c, 0x40, 0, 0, 0, 0xaa],
                    }),
                })
                .unwrap();
        }
        writer
            .append_one(RecordingFrame {
                received_at: started_at + Duration::from_millis(100),
                timestamp: Some(Duration::from_millis(100)),
                frame: MediaFrame::Video(VideoFrame {
                    codec: VideoCodec::H264,
                    is_keyframe: true,
                    width: 640,
                    height: 360,
                    data: payload,
                }),
            })
            .unwrap();
        writer.finalize().unwrap();
    }

    fn write_malformed_recording(path: &Path) {
        let config = mp4::Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
            timescale: 1_000,
        };
        let tracks = [
            mp4::TrackConfig {
                track_type: mp4::TrackType::Video,
                timescale: 90_000,
                language: "und".to_owned(),
                media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                    width: 320,
                    height: 240,
                    seq_param_set: vec![1],
                    pic_param_set: vec![2],
                }),
            },
            mp4::TrackConfig {
                track_type: mp4::TrackType::Audio,
                timescale: 16,
                language: "und".to_owned(),
                media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
                    bitrate: 64_000,
                    profile: mp4::AudioObjectType::AacLowComplexity,
                    freq_index: mp4::SampleFreqIndex::Freq16000,
                    chan_conf: mp4::ChannelConfig::Mono,
                }),
            },
        ];
        let mut writer =
            mp4::FragmentedMp4Writer::write_start(File::create(path).unwrap(), &config, &tracks)
                .unwrap();
        writer
            .write_sample(
                1,
                mp4::Mp4Sample {
                    start_time: 0,
                    duration: 90_000,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: vec![1].into(),
                },
            )
            .unwrap();
        writer
            .write_sample(
                2,
                mp4::Mp4Sample {
                    start_time: 0,
                    duration: 1_024,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: vec![2].into(),
                },
            )
            .unwrap();
        writer.write_end().unwrap();
    }
}
