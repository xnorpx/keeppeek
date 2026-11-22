use anyhow::{Context, Result, ensure};
use keeppeek::storage::{MediaFrame, RecordingFrame, VideoCodec, VideoFrame};
use std::time::{Duration, Instant};

pub fn parameter_sets(track: &mp4::Mp4Track, codec: VideoCodec) -> Result<Vec<u8>> {
    let sets = match codec {
        VideoCodec::H264 => {
            let avc = track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .avc1()
                .context("missing AVC configuration")?;
            ensure!(
                avc.avcc.length_size_minus_one == 3,
                "fixture requires four-byte NAL lengths"
            );
            vec![
                track.sequence_parameter_set()?.to_vec(),
                track.picture_parameter_set()?.to_vec(),
            ]
        }
        VideoCodec::H265 => {
            let entry = track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .hev1()
                .or_else(|| track.trak.mdia.minf.stbl.stsd.hvc1())
                .context("missing HEVC configuration")?;
            let config = entry.hvcc.configuration()?;
            ensure!(
                config.nal_length_size == 4,
                "fixture requires four-byte NAL lengths"
            );
            config
                .vps
                .into_iter()
                .chain(config.sps)
                .chain(config.pps)
                .collect()
        }
    };
    let mut data = Vec::new();
    for set in sets {
        data.extend_from_slice(&u32::try_from(set.len())?.to_be_bytes());
        data.extend_from_slice(&set);
    }
    Ok(data)
}

pub fn frame(
    codec: VideoCodec,
    dimensions: (u32, u32),
    sample: mp4::Mp4Sample,
    parameters: &[u8],
    received_at: Instant,
    timestamp: Duration,
) -> RecordingFrame {
    let mut data = if sample.is_sync {
        parameters.to_vec()
    } else {
        Vec::new()
    };
    data.extend_from_slice(&sample.bytes);
    RecordingFrame {
        source_rtp: None,
        received_at,
        timestamp: Some(timestamp),
        frame: MediaFrame::Video(VideoFrame {
            codec,
            is_keyframe: sample.is_sync,
            width: dimensions.0,
            height: dimensions.1,
            data: data.into(),
        }),
    }
}
