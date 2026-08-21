use anyhow::{Context, bail};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
};

use crate::storage::CatalogMediaFragment;

struct PlaybackTrack {
    source_id: u32,
    config: mp4::TrackConfig,
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
    sample: mp4::Mp4Sample,
}

pub fn export_fragment_ranges(
    fragments: &[CatalogMediaFragment],
    requested_end_ms: i64,
    destination: &Path,
    cancelled: impl Fn() -> bool,
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
    let result = write_export_recordings(&recordings, aligned_start_ms, &temporary, &cancelled)
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
) -> anyhow::Result<()> {
    let first = recordings
        .first()
        .context("export has no source recordings")?;
    let mut first_reader = mp4::read_mp4(File::open(&first.path)?)?;
    let tracks = export_tracks(&first_reader)?;
    let configs = tracks
        .iter()
        .map(|track| track.config.clone())
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
    let mut writer = mp4::FragmentedMp4Writer::write_start(output, &config, &configs)?;
    let mut pending_video = false;

    write_export_recording(
        &mut first_reader,
        first,
        &tracks,
        aligned_start_ms,
        &mut writer,
        &mut pending_video,
        cancelled,
    )?;
    for recording in recordings.iter().skip(1) {
        let mut reader = mp4::read_mp4(File::open(&recording.path)?)?;
        let source_tracks = export_tracks(&reader)?;
        if source_tracks
            .iter()
            .map(|track| &track.config)
            .ne(configs.iter())
        {
            bail!("export crosses a recording codec or track configuration change");
        }
        write_export_recording(
            &mut reader,
            recording,
            &source_tracks,
            aligned_start_ms,
            &mut writer,
            &mut pending_video,
            cancelled,
        )?;
    }
    writer.write_end()?;
    let mut output = writer.into_writer();
    output.flush()?;
    Ok(())
}

fn export_tracks<R: Read + Seek>(reader: &mp4::Mp4Reader<R>) -> anyhow::Result<Vec<PlaybackTrack>> {
    let mut tracks = reader
        .tracks()
        .iter()
        .filter_map(|(&source_id, track)| match playback_track_config(track) {
            Ok(Some(config)) => Some(Ok(PlaybackTrack { source_id, config })),
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
    pending_video: &mut bool,
    cancelled: &impl Fn() -> bool,
) -> anyhow::Result<()> {
    let mut samples = Vec::new();
    for (index, track) in tracks.iter().enumerate() {
        let sample_count = reader.sample_count(track.source_id)?;
        for sample_id in 1..=sample_count {
            if cancelled() {
                bail!("export was cancelled");
            }
            let mut sample = reader
                .read_sample(track.source_id, sample_id)?
                .with_context(|| {
                    format!("track {} sample {sample_id} is missing", track.source_id)
                })?;
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
            sample.start_time = u64::try_from(origin_ticks + i128::from(sample.start_time))
                .context("export sample timestamp precedes aligned start")?;
            samples.push(ExportSample {
                absolute_ms,
                track_id: u32::try_from(index + 1).unwrap_or(u32::MAX),
                track_type: track.config.track_type,
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
        if cancelled() {
            bail!("export was cancelled");
        }
        if sample.track_type == mp4::TrackType::Video && sample.sample.is_sync {
            if *pending_video {
                writer.flush_fragment()?;
            }
            *pending_video = true;
        }
        writer.write_sample(sample.track_id, sample.sample)?;
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
        .filter_map(|(&source_id, track)| match playback_track_config(track) {
            Ok(Some(config)) => Some(Ok(PlaybackTrack { source_id, config })),
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
        major_brand: "isom".parse().unwrap(),
        minor_version: 512,
        compatible_brands: vec![
            "isom".parse().unwrap(),
            "iso2".parse().unwrap(),
            "mp41".parse().unwrap(),
        ],
        timescale: 1_000,
    };
    let output = BufWriter::new(File::create(destination)?);
    let mut writer = mp4::Mp4Writer::write_start(output, &config)?;
    for track in &tracks {
        writer.add_track(&track.config)?;
    }
    for (output_index, track) in tracks.iter().enumerate() {
        let sample_count = reader.sample_count(track.source_id)?;
        for sample_id in 1..=sample_count {
            let sample = reader
                .read_sample(track.source_id, sample_id)?
                .with_context(|| {
                    format!("track {} sample {sample_id} is missing", track.source_id)
                })?;
            writer.write_sample(output_index as u32 + 1, &sample)?;
        }
    }
    writer.write_end()?;
    let mut output = writer.into_writer();
    output.flush()?;
    Ok(())
}

fn playback_track_config(track: &mp4::Mp4Track) -> anyhow::Result<Option<mp4::TrackConfig>> {
    let media_conf = match track.media_type()? {
        mp4::MediaType::H264 => mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
            width: track.width(),
            height: track.height(),
            seq_param_set: track.sequence_parameter_set()?.to_vec(),
            pic_param_set: track.picture_parameter_set()?.to_vec(),
        }),
        mp4::MediaType::H265 => {
            let sample_entry = track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .hev1
                .as_ref()
                .or(track.trak.mdia.minf.stbl.stsd.hvc1.as_ref())
                .context("HEVC track has no sample entry")?;
            let configuration = sample_entry.hvcc.configuration()?;
            mp4::MediaConfig::HevcConfig(mp4::HevcConfig {
                width: track.width(),
                height: track.height(),
                vps: configuration.vps.first().cloned().unwrap_or_default(),
                sps: configuration.sps.first().cloned().unwrap_or_default(),
                pps: configuration.pps.first().cloned().unwrap_or_default(),
            })
        }
        mp4::MediaType::AAC => {
            let frequency = track.sample_freq_index()?;
            return Ok(Some(mp4::TrackConfig {
                track_type: mp4::TrackType::Audio,
                timescale: frequency.freq(),
                language: track.language().to_owned(),
                media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
                    bitrate: track.bitrate().max(64_000),
                    profile: track.audio_profile()?,
                    freq_index: frequency,
                    chan_conf: track.channel_config()?,
                }),
            }));
        }
        _ => return Ok(None),
    };
    Ok(Some(mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: track.timescale(),
        language: track.language().to_owned(),
        media_conf,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::storage::{
        MediaFrame, RecordingCatalog, RecordingFrame, VideoCodec, VideoFrame,
        medium_term::MediumTermWriter,
    };
    use std::time::{Duration, Instant};

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
