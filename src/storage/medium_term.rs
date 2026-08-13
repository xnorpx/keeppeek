use crate::storage::{
    adts,
    frame::{AudioCodec, MediaFrame, VideoCodec},
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

const VIDEO_TIMESCALE: u32 = 90_000;
const AAC_SAMPLES_PER_FRAME: u32 = 1024;

pub struct MediumTermWriter {
    state: WriterState,
    path: PathBuf,
    final_path: PathBuf,
    started_at: Instant,
    segment_origin: Option<Instant>,
    camera_dts_origin: Option<u64>,
    frames_written: u64,
    bytes_written: u64,
    write_buffer_bytes: usize,
}

enum WriterState {
    WaitingForKeyframe,
    Preparing(Vec<RecordingFrame>),
    Active(ActiveWriter),
}

struct ActiveWriter {
    writer: mp4::FragmentedMp4Writer<BufWriter<File>>,
    video_track: u32,
    audio_track: Option<u32>,
    audio_timescale: u32,
    last_video_dts: Option<u64>,
    video_dts_source: VideoDtsSource,
    next_audio_dts: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoDtsSource {
    Camera,
    ReceivedTime,
}

impl MediumTermWriter {
    pub fn create(
        root: &Path,
        camera_id: &str,
        started_at: Instant,
        write_buffer_bytes: usize,
    ) -> std::io::Result<Self> {
        let started_at_utc = time::OffsetDateTime::now_utc() - started_at.elapsed();
        let active_path = layout::active_segment_path(root, camera_id, started_at_utc, "mp4");
        let final_path = layout::segment_path(root, camera_id, started_at_utc, "mp4");

        if let Some(parent) = active_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        Ok(Self {
            state: WriterState::WaitingForKeyframe,
            path: active_path,
            final_path,
            started_at,
            segment_origin: None,
            camera_dts_origin: None,
            frames_written: 0,
            bytes_written: 0,
            write_buffer_bytes,
        })
    }

    pub fn append_batch(&mut self, frames: Vec<RecordingFrame>) -> std::io::Result<()> {
        for rf in frames {
            self.append_one(rf)?;
        }
        Ok(())
    }

    pub fn append_one(&mut self, rf: RecordingFrame) -> std::io::Result<()> {
        if matches!(self.state, WriterState::WaitingForKeyframe) {
            if rf.frame.is_video_keyframe() {
                self.segment_origin = Some(rf.received_at);
                self.camera_dts_origin = rf.camera_dts_90k;
                self.state = WriterState::Preparing(vec![rf]);
            }
            return Ok(());
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
        Ok(())
    }

    fn activate_prepared(&mut self) -> std::io::Result<()> {
        let state = std::mem::replace(&mut self.state, WriterState::WaitingForKeyframe);
        let WriterState::Preparing(frames) = state else {
            self.state = state;
            return Ok(());
        };
        let active = self.init_mp4(&frames)?;
        self.state = WriterState::Active(active);
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

        let file = BufWriter::with_capacity(self.write_buffer_bytes, File::create(&self.path)?);

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

        let media_conf = match video.codec {
            VideoCodec::H264 => {
                let (sps, pps) = nal::extract_h264_sps_pps(&video.data);
                let (width, height) = sps
                    .as_deref()
                    .zip(pps.as_deref())
                    .and_then(|(sps, pps)| nal::h264_pixel_dimensions(sps, pps))
                    .unwrap_or((video.width as u16, video.height as u16));
                mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                    width,
                    height,
                    seq_param_set: sps.unwrap_or_default(),
                    pic_param_set: pps.unwrap_or_default(),
                })
            }
            VideoCodec::H265 => {
                let (vps, sps, pps) = nal::extract_h265_params(&video.data);
                mp4::MediaConfig::HevcConfig(mp4::HevcConfig {
                    width: video.width as u16,
                    height: video.height as u16,
                    vps: vps.unwrap_or_default(),
                    sps: sps.unwrap_or_default(),
                    pps: pps.unwrap_or_default(),
                })
            }
        };

        let video_config = mp4::TrackConfig {
            track_type: mp4::TrackType::Video,
            timescale: VIDEO_TIMESCALE,
            language: String::from("und"),
            media_conf,
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
        let writer = mp4::FragmentedMp4Writer::write_start(file, &mp4_config, &track_configs)
            .map_err(mp4_err)?;
        tracing::debug!(
            path = %self.path.display(),
            audio = audio.is_some(),
            init_bytes = writer.initialization().size,
            "fragmented MP4 segment initialized",
        );

        Ok(ActiveWriter {
            writer,
            video_track: 1,
            audio_track: audio.map(|_| 2),
            audio_timescale: audio.map_or(0, |(timescale, _)| timescale),
            last_video_dts: None,
            video_dts_source: VideoDtsSource::Camera,
            next_audio_dts: None,
        })
    }

    fn write_frame(&mut self, rf: RecordingFrame) -> std::io::Result<()> {
        let WriterState::Active(ref mut active) = self.state else {
            return Ok(());
        };
        let origin = self.segment_origin.unwrap();
        let elapsed = rf.received_at.saturating_duration_since(origin);

        match rf.frame {
            MediaFrame::Video(video) => {
                let camera_dts_available =
                    rf.camera_dts_90k.is_some() && self.camera_dts_origin.is_some();
                let camera_dts = camera_dts_or_fallback(
                    rf.camera_dts_90k,
                    self.camera_dts_origin,
                    elapsed,
                    VIDEO_TIMESCALE,
                );
                let received_dts = elapsed_ticks(elapsed, VIDEO_TIMESCALE);
                let default_duration = VIDEO_TIMESCALE / 30;
                let previous_dts = active.last_video_dts;
                let (dts, camera_clock_reset) = resolve_video_dts(
                    &mut active.video_dts_source,
                    previous_dts,
                    camera_dts,
                    received_dts,
                    camera_dts_available,
                    default_duration,
                );
                if camera_clock_reset {
                    tracing::warn!(
                        previous_dts = previous_dts.unwrap_or_default(),
                        camera_dts,
                        received_dts,
                        "camera video timestamp reset; using received-time DTS for current MP4 segment",
                    );
                }
                if video.is_keyframe && active.writer.has_pending_samples() {
                    active
                        .writer
                        .flush_fragment_before_sample(active.video_track, dts)
                        .map_err(mp4_err)?;
                }
                let duration = active
                    .last_video_dts
                    .and_then(|last| u32::try_from(dts - last).ok())
                    .filter(|duration| *duration > 0)
                    .unwrap_or(default_duration);
                let data_len = video.data.len();
                active
                    .writer
                    .write_sample(
                        active.video_track,
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
                let camera_dts = rf.camera_dts_90k.map(|_| {
                    camera_dts_or_fallback(
                        rf.camera_dts_90k,
                        self.camera_dts_origin,
                        elapsed,
                        active.audio_timescale,
                    )
                });
                let dts = match (active.next_audio_dts, camera_dts) {
                    (Some(next), Some(camera)) => next.max(camera),
                    (Some(next), None) => next,
                    (None, Some(camera)) => camera,
                    (None, None) => elapsed_ticks(elapsed, active.audio_timescale),
                };

                active
                    .writer
                    .write_sample(
                        track,
                        mp4::Mp4Sample {
                            start_time: dts,
                            duration: AAC_SAMPLES_PER_FRAME,
                            rendering_offset: 0,
                            is_sync: true,
                            bytes: Bytes::copy_from_slice(raw_aac),
                        },
                    )
                    .map_err(mp4_err)?;
                active.next_audio_dts = Some(dts + u64::from(AAC_SAMPLES_PER_FRAME));

                self.frames_written += 1;
                self.bytes_written += raw_aac.len() as u64;
            }
        }
        Ok(())
    }

    pub fn finalize(mut self) -> std::io::Result<PathBuf> {
        if matches!(self.state, WriterState::Preparing(_)) {
            self.activate_prepared()?;
        }
        let state = std::mem::replace(&mut self.state, WriterState::WaitingForKeyframe);
        match state {
            WriterState::Active(ActiveWriter { mut writer, .. }) => {
                tracing::debug!(path = %self.path.display(), "writing final MP4 fragment");
                writer.write_end().map_err(mp4_err)?;
                let mut inner = writer.into_writer();
                std::io::Write::flush(&mut inner)?;
                drop(inner);
                tracing::debug!(from = %self.path.display(), to = %self.final_path.display(), "renaming active segment");
                std::fs::rename(&self.path, &self.final_path)?;
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

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }
}

fn elapsed_ticks(elapsed: Duration, timescale: u32) -> u64 {
    (elapsed.as_secs_f64() * timescale as f64) as u64
}

fn resolve_video_dts(
    source: &mut VideoDtsSource,
    previous_dts: Option<u64>,
    camera_dts: u64,
    received_dts: u64,
    camera_dts_available: bool,
    default_duration: u32,
) -> (u64, bool) {
    let candidate_dts = match source {
        VideoDtsSource::Camera => camera_dts,
        VideoDtsSource::ReceivedTime => received_dts,
    };
    let Some(previous_dts) = previous_dts else {
        return (candidate_dts, false);
    };
    if candidate_dts > previous_dts {
        return (candidate_dts, false);
    }

    let camera_reset = matches!(source, VideoDtsSource::Camera)
        && camera_dts_available
        && previous_dts.saturating_sub(camera_dts) >= u64::from(VIDEO_TIMESCALE);
    if camera_reset {
        *source = VideoDtsSource::ReceivedTime;
        return (
            received_dts.max(previous_dts + u64::from(default_duration)),
            true,
        );
    }

    (previous_dts + u64::from(default_duration), false)
}

/// Use the camera's own DTS when available, falling back to wall-clock elapsed.
///
/// `camera_dts_90k` and `origin_90k` are both in 90 kHz units. When the
/// target timescale differs (e.g. audio at 16 kHz) we rescale accordingly.
fn camera_dts_or_fallback(
    camera_dts_90k: Option<u64>,
    origin_90k: Option<u64>,
    wall_elapsed: Duration,
    timescale: u32,
) -> u64 {
    if let (Some(dts), Some(origin)) = (camera_dts_90k, origin_90k) {
        let delta_90k = dts.saturating_sub(origin);
        if timescale == VIDEO_TIMESCALE {
            return delta_90k;
        }
        return (delta_90k as f64 * timescale as f64 / VIDEO_TIMESCALE as f64) as u64;
    }
    elapsed_ticks(wall_elapsed, timescale)
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
    use super::{VideoDtsSource, resolve_video_dts};

    #[test]
    fn reconnecting_camera_clock_switches_the_segment_to_received_time() {
        let mut source = VideoDtsSource::Camera;

        let (dts, reset) = resolve_video_dts(&mut source, Some(90_000), 0, 93_000, true, 3_000);

        assert_eq!(dts, 93_000);
        assert!(reset);
        assert_eq!(source, VideoDtsSource::ReceivedTime);

        let (dts, reset) = resolve_video_dts(&mut source, Some(dts), 3_000, 96_000, true, 3_000);

        assert_eq!(dts, 96_000);
        assert!(!reset);
    }

    #[test]
    fn monotonic_camera_clock_remains_the_segment_clock() {
        let mut source = VideoDtsSource::Camera;

        let (dts, reset) =
            resolve_video_dts(&mut source, Some(90_000), 93_000, 95_000, true, 3_000);

        assert_eq!(dts, 93_000);
        assert!(!reset);
        assert_eq!(source, VideoDtsSource::Camera);
    }
}
