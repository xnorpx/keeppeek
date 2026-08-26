use crate::media_time::{duration_to_ticks, ticks_to_duration};
use crate::storage::{
    adts,
    catalog::{CatalogFragment, CatalogKeyframe, CatalogRecording, RecordingCatalogHandle},
    frame::{AudioCodec, MediaFrame, VideoCodec, VideoFrame},
    identity::RecordingStreamIdentity,
    layout, nal,
    segment::RecordingFrame,
};
use bytes::Bytes;
use std::{
    fs::File,
    io::BufWriter,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

const VIDEO_TIMESCALE: u32 = 90_000;

pub struct MediumTermWriter {
    state: WriterState,
    path: PathBuf,
    final_path: PathBuf,
    recording_id: String,
    identity: RecordingStreamIdentity,
    started_at_ms: i64,
    catalog: Option<RecordingCatalogHandle>,
    started_at: Instant,
    segment_origin: Option<Instant>,
    camera_timestamp_origin: Option<Duration>,
    frames_written: u64,
    bytes_written: u64,
    recorded_duration: Duration,
    write_buffer_bytes: usize,
}

enum WriterState {
    WaitingForKeyframe,
    Preparing(Vec<RecordingFrame>),
    Active(Box<ActiveWriter>),
}

struct ActiveWriter {
    writer: mp4::FragmentedMp4Writer<BufWriter<File>>,
    video_track: u32,
    video_codec: VideoCodec,
    video_media_config: mp4::MediaConfig,
    video_sample_description_index: u32,
    audio_track: Option<u32>,
    audio_timescale: u32,
    last_video_dts: Option<u64>,
    last_video_duration: u32,
    fragment_start_dts: Option<u64>,
    next_audio_dts: Option<u64>,
}

impl MediumTermWriter {
    pub fn create(
        root: &Path,
        camera_id: &str,
        started_at: Instant,
        write_buffer_bytes: usize,
    ) -> std::io::Result<Self> {
        Self::create_inner(
            root,
            RecordingStreamIdentity::legacy(camera_id),
            started_at,
            write_buffer_bytes,
            None,
        )
    }

    pub fn create_with_catalog(
        root: &Path,
        camera_id: &str,
        started_at: Instant,
        write_buffer_bytes: usize,
        catalog: RecordingCatalogHandle,
    ) -> std::io::Result<Self> {
        Self::create_inner(
            root,
            RecordingStreamIdentity::legacy(camera_id),
            started_at,
            write_buffer_bytes,
            Some(catalog),
        )
    }

    pub fn create_with_catalog_identity(
        root: &Path,
        identity: RecordingStreamIdentity,
        started_at: Instant,
        write_buffer_bytes: usize,
        catalog: RecordingCatalogHandle,
    ) -> std::io::Result<Self> {
        Self::create_inner(
            root,
            identity,
            started_at,
            write_buffer_bytes,
            Some(catalog),
        )
    }

    fn create_inner(
        root: &Path,
        identity: RecordingStreamIdentity,
        started_at: Instant,
        write_buffer_bytes: usize,
        catalog: Option<RecordingCatalogHandle>,
    ) -> std::io::Result<Self> {
        let started_at_utc = time::OffsetDateTime::now_utc() - started_at.elapsed();
        let started_at_ms = i64::try_from(started_at_utc.unix_timestamp_nanos() / 1_000_000)
            .map_err(|_| std::io::Error::other("recording start timestamp is out of range"))?;
        let active_path =
            layout::active_segment_path(root, &identity.storage_key, started_at_utc, "mp4");
        let final_path = layout::segment_path(root, &identity.storage_key, started_at_utc, "mp4");

        if let Some(parent) = active_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        Ok(Self {
            state: WriterState::WaitingForKeyframe,
            path: active_path,
            final_path,
            recording_id: Uuid::new_v4().hyphenated().to_string(),
            identity,
            started_at_ms,
            catalog,
            started_at,
            segment_origin: None,
            camera_timestamp_origin: None,
            frames_written: 0,
            bytes_written: 0,
            recorded_duration: Duration::ZERO,
            write_buffer_bytes,
        })
    }

    pub fn append_batch(&mut self, frames: Vec<RecordingFrame>) -> std::io::Result<Duration> {
        let mut recorded_duration = Duration::ZERO;
        for rf in frames {
            recorded_duration = recorded_duration.saturating_add(self.append_one(rf)?);
        }
        Ok(recorded_duration)
    }

    pub fn append_one(&mut self, rf: RecordingFrame) -> std::io::Result<Duration> {
        let previous_duration = self.recorded_duration;
        if matches!(self.state, WriterState::WaitingForKeyframe) {
            if rf.frame.is_video_keyframe() {
                self.segment_origin = Some(rf.received_at);
                self.camera_timestamp_origin = rf.timestamp;
                self.state = WriterState::Preparing(vec![rf]);
            }
            return Ok(Duration::ZERO);
        }

        if matches!(self.state, WriterState::Preparing(_)) && rf.frame.is_video_keyframe() {
            self.activate_prepared()?;
        }
        match &mut self.state {
            WriterState::Preparing(frames) => frames.push(rf),
            WriterState::Active(_) => self.write_frame(rf)?,
            WriterState::WaitingForKeyframe => {
                return Err(std::io::Error::other(
                    "fragmented MP4 writer lost its prepared state",
                ));
            }
        }
        Ok(self.recorded_duration.saturating_sub(previous_duration))
    }

    fn activate_prepared(&mut self) -> std::io::Result<()> {
        let state = std::mem::replace(&mut self.state, WriterState::WaitingForKeyframe);
        let WriterState::Preparing(frames) = state else {
            self.state = state;
            return Ok(());
        };
        let active = self.init_mp4(&frames)?;
        self.state = WriterState::Active(Box::new(active));
        for frame in frames {
            self.write_frame(frame)?;
        }
        Ok(())
    }

    fn init_mp4(&self, frames: &[RecordingFrame]) -> std::io::Result<ActiveWriter> {
        let keyframe = frames
            .iter()
            .find(|frame| frame.frame.is_video_keyframe())
            .ok_or_else(|| std::io::Error::other("prepared MP4 segment has no keyframe"))?;
        let MediaFrame::Video(ref video) = keyframe.frame else {
            return Err(std::io::Error::other("prepared MP4 keyframe is not video"));
        };

        let mp4_config = mp4::Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec![
                "iso6".parse().unwrap(),
                "isom".parse().unwrap(),
                "mp41".parse().unwrap(),
            ],
            timescale: 1000,
        };

        let media_conf = required_video_media_config(video)?;

        let video_config = mp4::TrackConfig {
            track_type: mp4::TrackType::Video,
            timescale: VIDEO_TIMESCALE,
            language: String::from("und"),
            media_conf: media_conf.clone(),
        };
        let audio = frames.iter().find_map(|frame| {
            let MediaFrame::Audio(audio) = &frame.frame else {
                return None;
            };
            if audio.codec != AudioCodec::Aac {
                return None;
            }
            let (raw_aac, adts_info) = adts::strip_adts(&audio.data);
            if raw_aac.is_empty() {
                return None;
            }
            let timescale = adts_info
                .as_ref()
                .map_or(audio.sample_rate, |info| info.sample_rate);
            let channels = adts_info.as_ref().map_or(1, |info| info.channels);
            Some((timescale, channels))
        });
        let mut track_configs = vec![video_config];
        if let Some((timescale, channels)) = audio {
            track_configs.push(mp4::TrackConfig {
                track_type: mp4::TrackType::Audio,
                timescale,
                language: String::from("und"),
                media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
                    bitrate: 64_000,
                    profile: mp4::AudioObjectType::AacLowComplexity,
                    freq_index: sample_freq_index(timescale),
                    chan_conf: match channels {
                        2 => mp4::ChannelConfig::Stereo,
                        _ => mp4::ChannelConfig::Mono,
                    },
                }),
            });
        }
        let file = BufWriter::with_capacity(self.write_buffer_bytes, File::create(&self.path)?);
        let writer = mp4::FragmentedMp4Writer::write_start(file, &mp4_config, &track_configs)
            .map_err(mp4_err)?;
        let initialization = writer.initialization();
        if let Some(catalog) = &self.catalog {
            catalog
                .upsert_recording(CatalogRecording {
                    id: self.recording_id.clone(),
                    stream_id: self.identity.storage_key.clone(),
                    source_id: Some(self.identity.source_id.clone()),
                    logical_stream_id: Some(self.identity.stream_id.clone()),
                    started_at_ms: self.started_at_ms,
                    ended_at_ms: None,
                    path: self.path.to_string_lossy().into_owned(),
                    init_offset: initialization.offset,
                    init_len: initialization.size,
                    finalized: false,
                })
                .map_err(catalog_err)?;
        }
        tracing::debug!(
            path = %self.path.display(),
            audio = audio.is_some(),
            init_bytes = writer.initialization().size,
            "fragmented MP4 segment initialized",
        );

        Ok(ActiveWriter {
            writer,
            video_track: 1,
            video_codec: video.codec,
            video_media_config: media_conf,
            video_sample_description_index: 1,
            audio_track: audio.map(|_| 2),
            audio_timescale: audio.map_or(0, |(timescale, _)| timescale),
            last_video_dts: None,
            last_video_duration: 0,
            fragment_start_dts: None,
            next_audio_dts: None,
        })
    }

    fn write_frame(&mut self, rf: RecordingFrame) -> std::io::Result<()> {
        let WriterState::Active(ref mut active) = self.state else {
            return Ok(());
        };
        let origin = self.segment_origin.unwrap();
        let elapsed = rf.received_at.saturating_duration_since(origin);

        let mut completed_fragment = None;
        match rf.frame {
            MediaFrame::Video(video) => {
                let camera_dts = timestamp_ticks_or_fallback(
                    rf.timestamp,
                    self.camera_timestamp_origin,
                    elapsed,
                    VIDEO_TIMESCALE,
                );
                let fallback_duration = active.last_video_duration.max(1);
                let previous_dts = active.last_video_dts;
                let dts = resolve_video_dts(previous_dts, camera_dts, fallback_duration);
                if video.is_keyframe && active.writer.has_pending_samples() {
                    let fragment_start_dts = active.fragment_start_dts.ok_or_else(|| {
                        std::io::Error::other("active MP4 fragment has no start timestamp")
                    })?;
                    let fragment = active
                        .writer
                        .flush_fragment_before_sample(active.video_track, dts)
                        .map_err(mp4_err)?;
                    completed_fragment =
                        fragment.map(|fragment| (fragment, fragment_start_dts, dts));
                    active.fragment_start_dts = Some(dts);
                }
                if video.is_keyframe {
                    if let Some(media_config) = video_media_config(&video)? {
                        active.video_sample_description_index = active
                            .writer
                            .add_sample_description(active.video_track, media_config.clone())
                            .map_err(mp4_err)?;
                        active.video_codec = video.codec;
                        active.video_media_config = media_config;
                    } else if video.codec != active.video_codec {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "video codec changed without decoder parameters",
                        ));
                    }
                } else {
                    if video.codec != active.video_codec {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "video codec changed without a keyframe boundary",
                        ));
                    }
                    let has_parameters = match video.codec {
                        VideoCodec::H264 => nal::has_h264_parameter_sets(&video.data),
                        VideoCodec::H265 => nal::has_h265_parameter_sets(&video.data),
                    };
                    if has_parameters
                        && required_video_media_config(&video)? != active.video_media_config
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "video decoder parameters changed without a keyframe boundary",
                        ));
                    }
                }
                active.fragment_start_dts.get_or_insert(dts);
                let duration = active
                    .last_video_dts
                    .and_then(|last| u32::try_from(dts - last).ok())
                    .filter(|duration| *duration > 0)
                    .unwrap_or(fallback_duration);
                let data_len = video.data.len();
                active
                    .writer
                    .write_sample_with_description(
                        active.video_track,
                        active.video_sample_description_index,
                        mp4::Mp4Sample {
                            start_time: dts,
                            duration,
                            rendering_offset: 0,
                            is_sync: video.is_keyframe,
                            bytes: video.data,
                        },
                    )
                    .map_err(mp4_err)?;
                active.last_video_dts = Some(dts);
                active.last_video_duration = duration;
                self.recorded_duration = self
                    .recorded_duration
                    .saturating_add(ticks_to_duration(u64::from(duration), VIDEO_TIMESCALE));
                self.frames_written += 1;
                self.bytes_written += data_len as u64;
            }
            MediaFrame::Audio(audio) => {
                if audio.codec != AudioCodec::Aac {
                    return Ok(());
                }

                let (raw_aac, _) = adts::strip_adts(&audio.data);
                if raw_aac.is_empty() {
                    return Ok(());
                }

                let Some(track) = active.audio_track else {
                    return Ok(());
                };
                let camera_dts = rf.timestamp.map(|_| {
                    timestamp_ticks_or_fallback(
                        rf.timestamp,
                        self.camera_timestamp_origin,
                        elapsed,
                        active.audio_timescale,
                    )
                });
                let dts = match (active.next_audio_dts, camera_dts) {
                    (Some(next), Some(camera)) => next.max(camera),
                    (Some(next), None) => next,
                    (None, Some(camera)) => camera,
                    (None, None) => duration_to_ticks(elapsed, active.audio_timescale),
                };
                let sample_duration =
                    audio_sample_duration(audio.duration, active.audio_timescale)?;

                active
                    .writer
                    .write_sample(
                        track,
                        mp4::Mp4Sample {
                            start_time: dts,
                            duration: sample_duration,
                            rendering_offset: 0,
                            is_sync: true,
                            bytes: Bytes::copy_from_slice(raw_aac),
                        },
                    )
                    .map_err(mp4_err)?;
                active.next_audio_dts = Some(dts + u64::from(sample_duration));

                self.frames_written += 1;
                self.bytes_written += raw_aac.len() as u64;
            }
        }
        if let Some((fragment, start_dts, end_dts)) = completed_fragment {
            self.insert_catalog_fragment(fragment, start_dts, end_dts)?;
        }
        Ok(())
    }

    fn insert_catalog_fragment(
        &self,
        fragment: mp4::Mp4FragmentInfo,
        start_dts: u64,
        end_dts: u64,
    ) -> std::io::Result<()> {
        let Some(catalog) = &self.catalog else {
            return Ok(());
        };
        let start_offset_ms = ticks_to_millis(start_dts, VIDEO_TIMESCALE);
        let duration_ms =
            ticks_to_millis(end_dts.saturating_sub(start_dts), VIDEO_TIMESCALE).max(1);
        let start_ms = self
            .started_at_ms
            .saturating_add(i64::try_from(start_offset_ms).unwrap_or(i64::MAX));
        let catalog_fragment = CatalogFragment {
            recording_id: self.recording_id.clone(),
            sequence: u64::from(fragment.sequence_number),
            start_ms,
            duration_ms,
            byte_offset: fragment.range.offset,
            byte_len: fragment.range.size,
            random_access: true,
        };
        if let Some(location) = fragment.video_keyframe {
            catalog
                .insert_fragment_with_keyframe(
                    catalog_fragment,
                    CatalogKeyframe {
                        recording_id: self.recording_id.clone(),
                        fragment_sequence: u64::from(fragment.sequence_number),
                        byte_offset: location.offset,
                        byte_len: u64::from(location.size),
                    },
                )
                .map_err(catalog_err)
        } else {
            catalog
                .insert_fragment(catalog_fragment)
                .map_err(catalog_err)
        }
    }

    pub fn finalize(mut self) -> std::io::Result<PathBuf> {
        if matches!(self.state, WriterState::Preparing(_)) {
            self.activate_prepared()?;
        }
        let state = std::mem::replace(&mut self.state, WriterState::WaitingForKeyframe);
        match state {
            WriterState::Active(active) => {
                let ActiveWriter {
                    mut writer,
                    last_video_dts,
                    last_video_duration,
                    fragment_start_dts,
                    ..
                } = *active;
                tracing::debug!(path = %self.path.display(), "writing final MP4 fragment");
                let final_fragment = writer.write_end().map_err(mp4_err)?;
                let mut inner = writer.into_writer();
                std::io::Write::flush(&mut inner)?;
                drop(inner);
                if let Some(fragment) = final_fragment {
                    let start_dts = fragment_start_dts.ok_or_else(|| {
                        std::io::Error::other("final MP4 fragment has no start timestamp")
                    })?;
                    let end_dts = last_video_dts
                        .unwrap_or(start_dts)
                        .saturating_add(u64::from(last_video_duration));
                    self.insert_catalog_fragment(fragment, start_dts, end_dts)?;
                }
                tracing::debug!(from = %self.path.display(), to = %self.final_path.display(), "renaming active segment");
                std::fs::rename(&self.path, &self.final_path)?;
                if let Some(catalog) = &self.catalog {
                    catalog
                        .update_recording_path(&self.recording_id, &self.final_path, true)
                        .map_err(catalog_err)?;
                }
                Ok(self.final_path)
            }
            _ => {
                let _ = std::fs::remove_file(&self.path);
                Ok(self.final_path)
            }
        }
    }

    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    pub const fn frames_written(&self) -> u64 {
        self.frames_written
    }

    pub fn active_path(&self) -> &Path {
        &self.path
    }

    pub fn recording_id(&self) -> &str {
        &self.recording_id
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }
}

fn required_video_media_config(video: &VideoFrame) -> std::io::Result<mp4::MediaConfig> {
    video_media_config(video)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} keyframe has no decoder parameters", video.codec),
        )
    })
}

fn video_media_config(video: &VideoFrame) -> std::io::Result<Option<mp4::MediaConfig>> {
    match video.codec {
        VideoCodec::H264 => {
            let (sps, pps) = nal::extract_h264_sps_pps(&video.data);
            let (sps, pps) = match (sps, pps) {
                (None, None) => return Ok(None),
                (Some(sps), Some(pps)) => (sps, pps),
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "H.264 keyframe has incomplete decoder parameters",
                    ));
                }
            };
            let fallback_width = u16::try_from(video.width).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "H.264 width exceeds MP4 range",
                )
            })?;
            let fallback_height = u16::try_from(video.height).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "H.264 height exceeds MP4 range",
                )
            })?;
            let (width, height) =
                nal::h264_pixel_dimensions(&sps, &pps).unwrap_or((fallback_width, fallback_height));
            Ok(Some(mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                width,
                height,
                seq_param_set: sps,
                pic_param_set: pps,
            })))
        }
        VideoCodec::H265 => {
            let (vps, sps, pps) = nal::extract_h265_params(&video.data);
            let (vps, sps, pps) = match (vps, sps, pps) {
                (None, None, None) => return Ok(None),
                (Some(vps), Some(sps), Some(pps)) => (vps, sps, pps),
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "H.265 keyframe has incomplete decoder parameters",
                    ));
                }
            };
            let parameters = retina::codec::h265::parameters_from_vps_sps_pps(
                &vps,
                &sps,
                &pps,
                retina::codec::h26x::Framing::FourByteLength,
            )
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid H.265 parameter sets: {error}"),
                )
            })?;
            let (width, height) = parameters.pixel_dimensions();
            Ok(Some(mp4::MediaConfig::HevcConfig(mp4::HevcConfig {
                width: u16::try_from(width).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "H.265 width exceeds MP4 range",
                    )
                })?,
                height: u16::try_from(height).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "H.265 height exceeds MP4 range",
                    )
                })?,
                vps,
                sps,
                pps,
                decoder_config: parameters.extra_data().to_vec(),
            })))
        }
    }
}

fn resolve_video_dts(previous_dts: Option<u64>, camera_dts: u64, fallback_duration: u32) -> u64 {
    let Some(previous_dts) = previous_dts else {
        return camera_dts;
    };
    if camera_dts > previous_dts {
        return camera_dts;
    }

    previous_dts.saturating_add(u64::from(fallback_duration.max(1)))
}

fn timestamp_ticks_or_fallback(
    timestamp: Option<Duration>,
    origin: Option<Duration>,
    wall_elapsed: Duration,
    timescale: u32,
) -> u64 {
    let elapsed = timestamp
        .zip(origin)
        .map_or(wall_elapsed, |(timestamp, origin)| {
            timestamp.saturating_sub(origin)
        });
    duration_to_ticks(elapsed, timescale)
}

fn audio_sample_duration(duration: Duration, timescale: u32) -> std::io::Result<u32> {
    u32::try_from(duration_to_ticks(duration, timescale))
        .ok()
        .filter(|duration| *duration > 0)
        .ok_or_else(|| std::io::Error::other("invalid audio frame duration"))
}

fn ticks_to_millis(ticks: u64, timescale: u32) -> u64 {
    let millis = u128::from(ticks).saturating_mul(1_000) / u128::from(timescale);
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn catalog_err(error: anyhow::Error) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

fn mp4_err(e: mp4::Error) -> std::io::Error {
    match e {
        mp4::Error::IoError(io_err) => io_err,
        other => std::io::Error::other(other.to_string()),
    }
}

const fn sample_freq_index(rate: u32) -> mp4::SampleFreqIndex {
    match rate {
        96000 => mp4::SampleFreqIndex::Freq96000,
        88200 => mp4::SampleFreqIndex::Freq88200,
        64000 => mp4::SampleFreqIndex::Freq64000,
        48000 => mp4::SampleFreqIndex::Freq48000,
        44100 => mp4::SampleFreqIndex::Freq44100,
        32000 => mp4::SampleFreqIndex::Freq32000,
        24000 => mp4::SampleFreqIndex::Freq24000,
        22050 => mp4::SampleFreqIndex::Freq22050,
        16000 => mp4::SampleFreqIndex::Freq16000,
        12000 => mp4::SampleFreqIndex::Freq12000,
        11025 => mp4::SampleFreqIndex::Freq11025,
        8000 => mp4::SampleFreqIndex::Freq8000,
        _ => mp4::SampleFreqIndex::Freq48000,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MediumTermWriter, audio_sample_duration, required_video_media_config, resolve_video_dts,
        timestamp_ticks_or_fallback,
    };
    use crate::storage::{
        AudioCodec, AudioFrame, MediaFrame, RecordingCatalog, RecordingFrame, VideoCodec,
        VideoFrame,
    };
    use bytes::Bytes;
    use std::{
        fs::File,
        io::{Read, Seek, SeekFrom},
        time::{Duration, Instant},
    };

    fn video_frame(received_at: Instant, timestamp: Duration) -> RecordingFrame {
        RecordingFrame {
            received_at,
            timestamp: Some(timestamp),
            frame: MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H264,
                is_keyframe: true,
                width: 320,
                height: 240,
                data: Bytes::from_static(&[
                    0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68,
                    0xce, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
                ]),
            }),
        }
    }

    fn incomplete_video_frame(received_at: Instant, timestamp: Duration) -> RecordingFrame {
        RecordingFrame {
            received_at,
            timestamp: Some(timestamp),
            frame: MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H264,
                is_keyframe: true,
                width: 320,
                height: 240,
                data: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
            }),
        }
    }

    fn h265_keyframe() -> (Bytes, mp4::Mp4VideoDecoderConfig) {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata/cc-4k-640x360-h265.mp4");
        let mut input = mp4::read_mp4(File::open(fixture).unwrap()).unwrap();
        let (&track_id, track) = input
            .tracks()
            .iter()
            .find(|(_, track)| matches!(track.media_type(), Ok(mp4::MediaType::H265)))
            .unwrap();
        let decoder = track.video_decoder_config().unwrap().unwrap();
        let sample_entry = track
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .hvc1()
            .or_else(|| track.trak.mdia.minf.stbl.stsd.hev1())
            .unwrap();
        let configuration = sample_entry.hvcc.configuration().unwrap();
        let sample = (1..=track.sample_count())
            .find_map(|sample_id| {
                let sample = input.read_sample(track_id, sample_id).unwrap().unwrap();
                sample.is_sync.then_some(sample.bytes)
            })
            .unwrap();
        let mut frame_data = Vec::new();
        for parameter_set in [
            &configuration.vps[0],
            &configuration.sps[0],
            &configuration.pps[0],
        ] {
            frame_data
                .extend_from_slice(&u32::try_from(parameter_set.len()).unwrap().to_be_bytes());
            frame_data.extend_from_slice(parameter_set);
        }
        frame_data.extend_from_slice(&sample);
        (Bytes::from(frame_data), decoder)
    }

    fn h264_keyframe(name: &str) -> (Bytes, mp4::Mp4VideoDecoderConfig) {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata")
            .join(name);
        let mut input = mp4::read_mp4(File::open(fixture).unwrap()).unwrap();
        let (&track_id, track) = input
            .tracks()
            .iter()
            .find(|(_, track)| matches!(track.media_type(), Ok(mp4::MediaType::H264)))
            .unwrap();
        let decoder = track.video_decoder_config().unwrap().unwrap();
        let config = track.media_config_for_description(1).unwrap();
        let mp4::MediaConfig::AvcConfig(config) = config else {
            panic!("H.264 fixture must expose an AVC configuration");
        };
        let sample = (1..=track.sample_count())
            .find_map(|sample_id| {
                let sample = input.read_sample(track_id, sample_id).unwrap().unwrap();
                sample.is_sync.then_some(sample.bytes)
            })
            .unwrap();
        let mut frame_data = Vec::new();
        for parameter_set in [&config.seq_param_set, &config.pic_param_set] {
            frame_data
                .extend_from_slice(&u32::try_from(parameter_set.len()).unwrap().to_be_bytes());
            frame_data.extend_from_slice(parameter_set);
        }
        frame_data.extend_from_slice(&sample);
        (Bytes::from(frame_data), decoder)
    }

    #[test]
    fn writer_waits_for_complete_decoder_parameters() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-parameters-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let mut writer = MediumTermWriter::create(&root, "camera/main", started_at, 8 * 1024)
            .expect("create writer");

        writer
            .append_one(incomplete_video_frame(started_at, Duration::ZERO))
            .unwrap();
        let error = writer
            .append_one(video_frame(
                started_at + Duration::from_secs(1),
                Duration::from_secs(1),
            ))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "h264 keyframe has no decoder parameters");
        assert!(!writer.active_path().exists());

        writer
            .append_one(video_frame(
                started_at + Duration::from_secs(2),
                Duration::from_secs(2),
            ))
            .unwrap();
        writer
            .append_one(video_frame(
                started_at + Duration::from_secs(3),
                Duration::from_secs(3),
            ))
            .unwrap();
        let path = writer.finalize().unwrap();
        assert!(path.is_file());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_reports_only_committed_video_duration() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-recorded-duration-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let mut writer = MediumTermWriter::create(&root, "camera/main", started_at, 8 * 1024)
            .expect("create writer");

        assert_eq!(
            writer
                .append_one(video_frame(started_at, Duration::ZERO))
                .unwrap(),
            Duration::ZERO
        );
        let prepared_duration = writer
            .append_one(video_frame(
                started_at + Duration::from_secs(1),
                Duration::from_secs(1),
            ))
            .unwrap();
        assert!(prepared_duration >= Duration::from_secs(1));
        assert!(prepared_duration < Duration::from_millis(1_001));
        assert_eq!(
            writer
                .append_one(video_frame(
                    started_at + Duration::from_secs(2),
                    Duration::from_secs(2),
                ))
                .unwrap(),
            Duration::from_secs(1)
        );

        writer.finalize().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_reuses_active_description_when_a_later_keyframe_omits_parameters() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-repeated-parameters-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let mut writer = MediumTermWriter::create(&root, "camera/sub", started_at, 8 * 1024)
            .expect("create writer");
        writer
            .append_one(video_frame(started_at, Duration::ZERO))
            .unwrap();
        writer
            .append_one(video_frame(
                started_at + Duration::from_millis(40),
                Duration::from_millis(40),
            ))
            .unwrap();
        writer
            .append_one(RecordingFrame {
                received_at: started_at + Duration::from_millis(80),
                timestamp: Some(Duration::from_millis(80)),
                frame: MediaFrame::Video(VideoFrame {
                    codec: VideoCodec::H264,
                    is_keyframe: true,
                    width: 320,
                    height: 240,
                    data: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
                }),
            })
            .unwrap();

        let path = writer.finalize().unwrap();
        let reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
        let video = reader
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
            .unwrap();
        assert_eq!(video.sample_description_count(), 1);
        assert_eq!(video.sample_count(), 3);
        assert_eq!(video.sample_description_index(3).unwrap(), 1);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_reuses_active_h265_description_when_a_later_keyframe_omits_parameters() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-repeated-h265-parameters-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let (full_keyframe, decoder) = h265_keyframe();
        let mut position = 0usize;
        for _ in 0..3 {
            let length =
                u32::from_be_bytes(full_keyframe[position..position + 4].try_into().unwrap())
                    as usize;
            position += 4 + length;
        }
        let parameterless_keyframe = full_keyframe.slice(position..);
        let mut writer = MediumTermWriter::create(&root, "camera/main", started_at, 8 * 1024)
            .expect("create writer");
        for offset_ms in [0, 40] {
            writer
                .append_one(RecordingFrame {
                    received_at: started_at + Duration::from_millis(offset_ms),
                    timestamp: Some(Duration::from_millis(offset_ms)),
                    frame: MediaFrame::Video(VideoFrame {
                        codec: VideoCodec::H265,
                        is_keyframe: true,
                        width: u32::from(decoder.width),
                        height: u32::from(decoder.height),
                        data: full_keyframe.clone(),
                    }),
                })
                .unwrap();
        }
        writer
            .append_one(RecordingFrame {
                received_at: started_at + Duration::from_millis(80),
                timestamp: Some(Duration::from_millis(80)),
                frame: MediaFrame::Video(VideoFrame {
                    codec: VideoCodec::H265,
                    is_keyframe: true,
                    width: u32::from(decoder.width),
                    height: u32::from(decoder.height),
                    data: parameterless_keyframe,
                }),
            })
            .unwrap();

        let path = writer.finalize().unwrap();
        let reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
        let video = reader
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
            .unwrap();
        assert_eq!(video.sample_description_count(), 1);
        assert_eq!(video.sample_count(), 3);
        assert_eq!(video.sample_description_index(3).unwrap(), 1);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_keeps_dts_increasing_across_backward_and_duplicate_camera_timestamps() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-backward-clock-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let mut writer = MediumTermWriter::create(&root, "camera/sub", started_at, 8 * 1024)
            .expect("create writer");
        for (received_ms, timestamp_ms) in [(0, 0), (40, 40), (80, 20), (120, 20)] {
            writer
                .append_one(video_frame(
                    started_at + Duration::from_millis(received_ms),
                    Duration::from_millis(timestamp_ms),
                ))
                .unwrap();
        }
        let path = writer.finalize().unwrap();
        let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
        let (&track_id, track) = reader
            .tracks()
            .iter()
            .find(|(_, track)| track.track_type().ok() == Some(mp4::TrackType::Video))
            .unwrap();
        let start_times = (1..=track.sample_count())
            .map(|sample_id| {
                reader
                    .read_sample(track_id, sample_id)
                    .unwrap()
                    .unwrap()
                    .start_time
            })
            .collect::<Vec<_>>();
        assert_eq!(start_times, vec![0, 3_600, 7_200, 10_800]);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_preserves_real_h265_decoder_configuration() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata/cc-4k-640x360-h265.mp4");
        let mut input = mp4::read_mp4(File::open(fixture).unwrap()).unwrap();
        let (&track_id, track) = input
            .tracks()
            .iter()
            .find(|(_, track)| matches!(track.media_type(), Ok(mp4::MediaType::H265)))
            .unwrap();
        let expected_decoder = track.video_decoder_config().unwrap().unwrap();
        let sample_entry = track
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .hev1()
            .or_else(|| track.trak.mdia.minf.stbl.stsd.hvc1())
            .unwrap();
        let configuration = sample_entry.hvcc.configuration().unwrap();
        let sample_count = track.sample_count();
        let mut sample = None;
        for sample_id in 1..=sample_count {
            let candidate = input.read_sample(track_id, sample_id).unwrap().unwrap();
            if candidate.is_sync {
                sample = Some(candidate.bytes);
                break;
            }
        }
        let mut frame_data = Vec::new();
        for parameter_set in [
            &configuration.vps[0],
            &configuration.sps[0],
            &configuration.pps[0],
        ] {
            frame_data
                .extend_from_slice(&u32::try_from(parameter_set.len()).unwrap().to_be_bytes());
            frame_data.extend_from_slice(parameter_set);
        }
        frame_data.extend_from_slice(&sample.unwrap());

        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-h265-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let mut writer = MediumTermWriter::create(&root, "camera/main", started_at, 8 * 1024)
            .expect("create writer");
        for offset_ms in [0, 1_000] {
            writer
                .append_one(RecordingFrame {
                    received_at: started_at + Duration::from_millis(offset_ms),
                    timestamp: Some(Duration::from_millis(offset_ms)),
                    frame: MediaFrame::Video(VideoFrame {
                        codec: VideoCodec::H265,
                        is_keyframe: true,
                        width: expected_decoder.width.into(),
                        height: expected_decoder.height.into(),
                        data: Bytes::from(frame_data.clone()),
                    }),
                })
                .unwrap();
        }
        let output_path = writer.finalize().unwrap();
        let output = mp4::read_mp4(File::open(output_path).unwrap()).unwrap();
        let output_track = output
            .tracks()
            .values()
            .find(|track| matches!(track.media_type(), Ok(mp4::MediaType::H265)))
            .unwrap();
        let decoder = output_track.video_decoder_config().unwrap().unwrap();
        let output_sample_entry = output_track
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .hev1()
            .or_else(|| output_track.trak.mdia.minf.stbl.stsd.hvc1())
            .unwrap();
        let output_configuration = output_sample_entry.hvcc.configuration().unwrap();
        assert_eq!(
            decoder.codec.strip_prefix("hev1"),
            expected_decoder.codec.strip_prefix("hvc1")
        );
        assert_eq!(decoder.width, expected_decoder.width);
        assert_eq!(decoder.height, expected_decoder.height);
        assert_eq!(decoder.nal_length_size, 4);
        assert_eq!(output_configuration.vps, configuration.vps);
        assert_eq!(output_configuration.sps, configuration.sps);
        assert_eq!(output_configuration.pps, configuration.pps);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_switches_codec_and_resolution_per_gop_without_assuming_frame_rate() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-variable-gop-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let (h265_data, h265_decoder) = h265_keyframe();
        let mut writer = MediumTermWriter::create(&root, "camera/sub", started_at, 8 * 1024)
            .expect("create writer");
        let frames = [
            video_frame(started_at, Duration::ZERO),
            RecordingFrame {
                received_at: started_at + Duration::from_millis(41),
                timestamp: Some(Duration::from_millis(41)),
                frame: MediaFrame::Video(VideoFrame {
                    codec: VideoCodec::H265,
                    is_keyframe: true,
                    width: u32::from(h265_decoder.width),
                    height: u32::from(h265_decoder.height),
                    data: h265_data.clone(),
                }),
            },
            RecordingFrame {
                received_at: started_at + Duration::from_millis(97),
                timestamp: Some(Duration::from_millis(97)),
                frame: MediaFrame::Video(VideoFrame {
                    codec: VideoCodec::H265,
                    is_keyframe: true,
                    width: u32::from(h265_decoder.width),
                    height: u32::from(h265_decoder.height),
                    data: h265_data,
                }),
            },
            video_frame(
                started_at + Duration::from_millis(130),
                Duration::from_millis(130),
            ),
        ];
        for frame in frames {
            writer.append_one(frame).unwrap();
        }

        let path = writer.finalize().unwrap();
        let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
        let (&track_id, track) = reader
            .tracks()
            .iter()
            .find(|(_, track)| matches!(track.track_type(), Ok(mp4::TrackType::Video)))
            .unwrap();
        assert_eq!(track.sample_description_count(), 2);
        assert_eq!(track.sample_description_index(1).unwrap(), 1);
        assert_eq!(track.sample_description_index(2).unwrap(), 2);
        assert_eq!(track.sample_description_index(3).unwrap(), 2);
        assert_eq!(track.sample_description_index(4).unwrap(), 1);
        assert_eq!(
            track.media_type_for_description(1).unwrap(),
            mp4::MediaType::H264
        );
        assert_eq!(
            track.media_type_for_description(2).unwrap(),
            mp4::MediaType::H265
        );
        assert_eq!(
            track.dimensions_for_description(2).unwrap(),
            (h265_decoder.width, h265_decoder.height)
        );
        assert_eq!(
            reader.read_sample(track_id, 1).unwrap().unwrap().duration,
            3_690
        );
        assert_eq!(
            reader.read_sample(track_id, 2).unwrap().unwrap().duration,
            5_040
        );
        assert_eq!(
            reader.read_sample(track_id, 3).unwrap().unwrap().duration,
            2_970
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_switches_h264_configuration_and_resolution_per_gop() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-h264-reconfiguration-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let (low_data, low_decoder) = h264_keyframe("cc-4k-640x360-h264.mp4");
        let (high_data, high_decoder) = h264_keyframe("cc-4k-3840x2160-h264.mp4");
        let mut writer = MediumTermWriter::create(&root, "camera/sub", started_at, 8 * 1024)
            .expect("create writer");
        for (offset_ms, data, decoder) in [
            (0, low_data.clone(), &low_decoder),
            (40, low_data.clone(), &low_decoder),
            (80, high_data, &high_decoder),
            (120, low_data, &low_decoder),
        ] {
            writer
                .append_one(RecordingFrame {
                    received_at: started_at + Duration::from_millis(offset_ms),
                    timestamp: Some(Duration::from_millis(offset_ms)),
                    frame: MediaFrame::Video(VideoFrame {
                        codec: VideoCodec::H264,
                        is_keyframe: true,
                        width: u32::from(decoder.width),
                        height: u32::from(decoder.height),
                        data,
                    }),
                })
                .unwrap();
        }

        let path = writer.finalize().unwrap();
        let reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
        let video = reader
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
            .unwrap();
        assert_eq!(video.sample_description_count(), 2);
        assert_eq!(video.sample_description_index(1).unwrap(), 1);
        assert_eq!(video.sample_description_index(2).unwrap(), 1);
        assert_eq!(video.sample_description_index(3).unwrap(), 2);
        assert_eq!(video.sample_description_index(4).unwrap(), 1);
        assert_eq!(
            video.dimensions_for_description(1).unwrap(),
            (low_decoder.width, low_decoder.height)
        );
        assert_eq!(
            video.dimensions_for_description(2).unwrap(),
            (high_decoder.width, high_decoder.height)
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_rejects_decoder_parameter_change_on_a_non_keyframe() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-non-keyframe-config-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let (low_data, low_decoder) = h264_keyframe("cc-4k-640x360-h264.mp4");
        let (high_data, high_decoder) = h264_keyframe("cc-4k-3840x2160-h264.mp4");
        let mut writer = MediumTermWriter::create(&root, "camera/sub", started_at, 8 * 1024)
            .expect("create writer");
        for offset_ms in [0, 40] {
            writer
                .append_one(RecordingFrame {
                    received_at: started_at + Duration::from_millis(offset_ms),
                    timestamp: Some(Duration::from_millis(offset_ms)),
                    frame: MediaFrame::Video(VideoFrame {
                        codec: VideoCodec::H264,
                        is_keyframe: true,
                        width: u32::from(low_decoder.width),
                        height: u32::from(low_decoder.height),
                        data: low_data.clone(),
                    }),
                })
                .unwrap();
        }
        let error = writer
            .append_one(RecordingFrame {
                received_at: started_at + Duration::from_millis(80),
                timestamp: Some(Duration::from_millis(80)),
                frame: MediaFrame::Video(VideoFrame {
                    codec: VideoCodec::H264,
                    is_keyframe: false,
                    width: u32::from(high_decoder.width),
                    height: u32::from(high_decoder.height),
                    data: high_data,
                }),
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "video decoder parameters changed without a keyframe boundary"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_partial_decoder_parameter_sets() {
        let h264 = VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: true,
            width: 640,
            height: 360,
            data: Bytes::from_static(&[0, 0, 0, 4, 0x67, 0x42, 0x00, 0x1f]),
        };
        assert_eq!(
            required_video_media_config(&h264).unwrap_err().to_string(),
            "H.264 keyframe has incomplete decoder parameters"
        );
        let h265 = VideoFrame {
            codec: VideoCodec::H265,
            is_keyframe: true,
            width: 1920,
            height: 1080,
            data: Bytes::from_static(&[0, 0, 0, 3, 0x40, 0x01, 0x0c, 0, 0, 0, 3, 0x42, 0x01, 0x01]),
        };
        assert_eq!(
            required_video_media_config(&h265).unwrap_err().to_string(),
            "H.265 keyframe has incomplete decoder parameters"
        );
    }

    #[test]
    fn writer_persists_protocol_audio_timestamp_and_duration() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-audio-{}",
            rand::random::<u64>()
        ));
        let started_at = Instant::now();
        let mut writer = MediumTermWriter::create(&root, "camera/main", started_at, 8 * 1024)
            .expect("create writer");

        writer
            .append_one(video_frame(started_at, Duration::ZERO))
            .unwrap();
        writer
            .append_one(RecordingFrame {
                received_at: started_at + Duration::from_millis(10),
                timestamp: Some(Duration::from_millis(10)),
                frame: MediaFrame::Audio(AudioFrame {
                    codec: AudioCodec::Aac,
                    sample_rate: 48_000,
                    duration: Duration::from_millis(20),
                    data: vec![0xFF, 0xF1, 0x4C, 0x40, 0, 0, 0, 0xAA],
                }),
            })
            .unwrap();
        writer
            .append_one(RecordingFrame {
                received_at: started_at + Duration::from_millis(80),
                timestamp: None,
                frame: MediaFrame::Audio(AudioFrame {
                    codec: AudioCodec::Aac,
                    sample_rate: 48_000,
                    duration: Duration::from_millis(20),
                    data: vec![0xFF, 0xF1, 0x4C, 0x40, 0, 0, 0, 0xBB],
                }),
            })
            .unwrap();
        writer
            .append_one(RecordingFrame {
                received_at: started_at + Duration::from_millis(100),
                timestamp: Some(Duration::from_millis(100)),
                frame: MediaFrame::Audio(AudioFrame {
                    codec: AudioCodec::Aac,
                    sample_rate: 48_000,
                    duration: Duration::from_millis(20),
                    data: vec![0xFF, 0xF1, 0x4C, 0x40, 0, 0, 0, 0xCC],
                }),
            })
            .unwrap();
        writer
            .append_one(video_frame(
                started_at + Duration::from_millis(134),
                Duration::from_millis(134),
            ))
            .unwrap();

        let path = writer.finalize().unwrap();
        let mut reader = mp4::read_mp4(File::open(&path).unwrap()).unwrap();
        let first_audio = reader.read_sample(2, 1).unwrap().unwrap();
        let untimestamped_audio = reader.read_sample(2, 2).unwrap().unwrap();
        let gap_audio = reader.read_sample(2, 3).unwrap().unwrap();

        assert_eq!(first_audio.start_time, 480);
        assert_eq!(first_audio.duration, 960);
        assert_eq!(untimestamped_audio.start_time, 1_440);
        assert_eq!(untimestamped_audio.duration, 3_360);
        assert_eq!(gap_audio.start_time, 4_800);
        assert_eq!(gap_audio.duration, 960);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_indexes_exact_initialization_and_fragment_ranges() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-medium-term-catalog-{}",
            rand::random::<u64>()
        ));
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        let started_at = Instant::now();
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let mut writer = MediumTermWriter::create_with_catalog(
            &root,
            "camera/main",
            started_at,
            8 * 1024,
            handle.clone(),
        )
        .unwrap();
        for offset_ms in [0, 1_000, 2_000] {
            writer
                .append_one(video_frame(
                    started_at + Duration::from_millis(offset_ms),
                    Duration::from_millis(offset_ms),
                ))
                .unwrap();
        }

        let path = writer.finalize().unwrap();
        let fragments = handle
            .media_fragments_in_range("camera/main", now_ms - 10_000, now_ms + 10_000)
            .unwrap();

        assert_eq!(fragments.len(), 3);
        assert!(
            fragments
                .iter()
                .all(|fragment| fragment.path == path.to_string_lossy())
        );
        assert_eq!(fragments[0].duration_ms, 1_000);
        let mut file = File::open(path).unwrap();
        let mut initialization = vec![0; usize::try_from(fragments[0].init_len).unwrap()];
        file.seek(SeekFrom::Start(fragments[0].init_offset))
            .unwrap();
        file.read_exact(&mut initialization).unwrap();
        assert_eq!(&initialization[4..8], b"ftyp");
        for fragment in &fragments {
            let mut header = [0; 8];
            file.seek(SeekFrom::Start(fragment.byte_offset)).unwrap();
            file.read_exact(&mut header).unwrap();
            assert_eq!(&header[4..8], b"moof");
        }

        drop(handle);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn audio_sample_duration_uses_protocol_frame_duration() {
        assert_eq!(
            audio_sample_duration(Duration::from_millis(20), 48_000).unwrap(),
            960
        );
        assert_eq!(
            audio_sample_duration(Duration::from_millis(64), 16_000).unwrap(),
            1_024
        );
    }

    #[test]
    fn non_advancing_camera_clock_uses_the_last_observed_frame_duration() {
        assert_eq!(resolve_video_dts(Some(90_000), 0, 3_000), 93_000);
    }

    #[test]
    fn monotonic_camera_clock_remains_the_segment_clock() {
        assert_eq!(resolve_video_dts(Some(90_000), 93_000, 3_000), 93_000);
    }

    #[test]
    fn missing_camera_timestamp_uses_the_receive_clock() {
        assert_eq!(
            timestamp_ticks_or_fallback(None, None, Duration::from_millis(41), 90_000),
            3_690
        );
    }

    #[test]
    fn timestamp_fallback_saturates_instead_of_overflowing() {
        assert_eq!(resolve_video_dts(Some(u64::MAX - 5), 0, 10), u64::MAX);
    }
}
