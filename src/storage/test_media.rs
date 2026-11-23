//! Checked-in camera footage as length-prefixed recording frames.

use super::{MediaFrame, RecordingFrame, VideoCodec, VideoFrame};
use anyhow::Context;
use std::{
    fs::File,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) fn video_fixture(codec: VideoCodec, resolution: &str) -> Vec<RecordingFrame> {
    let suffix = match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "crates/test-camera/testdata/cc-4k-{resolution}-{suffix}.mp4"
    ));
    read_video_fixture(&path, codec)
        .unwrap_or_else(|error| panic!("fixture {}: {error:#}", path.display()))
}

fn read_video_fixture(path: &Path, codec: VideoCodec) -> anyhow::Result<Vec<RecordingFrame>> {
    let mut reader =
        mp4::read_mp4(File::open(path).context("open fixture")?).context("parse MP4")?;
    let (&id, track) = reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().ok() == Some(mp4::TrackType::Video))
        .context("missing video track")?;
    let decoder = track
        .video_decoder_config()?
        .context("missing video decoder configuration")?;
    let config = track.media_config_for_description(1)?;
    let parameters: Vec<Vec<u8>> = match config {
        mp4::MediaConfig::AvcConfig(config) => vec![config.seq_param_set, config.pic_param_set],
        mp4::MediaConfig::HevcConfig(config) => vec![config.vps, config.sps, config.pps],
        _ => anyhow::bail!("fixture must use H.264 or H.265"),
    };
    let timescale = track.timescale();
    let count = track.sample_count();
    anyhow::ensure!(
        count > 0 && timescale > 0,
        "video track has no samples or timescale"
    );
    let start = Instant::now() - Duration::from_secs(60);
    (0..count * 6)
        .map(|index| {
            let number = index % count + 1;
            let sample = reader
                .read_sample(id, number)
                .with_context(|| format!("read video sample {number}"))?
                .with_context(|| format!("missing video sample {number}"))?;
            let mut bytes = Vec::new();
            if sample.is_sync {
                for parameter in &parameters {
                    bytes.extend_from_slice(
                        &u32::try_from(parameter.len())
                            .context("codec parameter size")?
                            .to_be_bytes(),
                    );
                    bytes.extend_from_slice(parameter);
                }
            }
            bytes.extend_from_slice(&sample.bytes);
            Ok(RecordingFrame {
                source_rtp: None,
                received_at: start
                    + Duration::from_secs(u64::from(index / count))
                    + Duration::from_secs_f64(sample.start_time as f64 / f64::from(timescale)),
                timestamp: None,
                frame: MediaFrame::Video(VideoFrame {
                    codec,
                    is_keyframe: sample.is_sync,
                    width: u32::from(decoder.width),
                    height: u32::from(decoder.height),
                    data: bytes.into(),
                }),
            })
        })
        .collect()
}
