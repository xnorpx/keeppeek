use anyhow::{Context, anyhow, bail};
use mp4::MediaType;
use std::{fs::File, path::Path, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
}

impl Codec {
    pub(crate) const fn onvif_name(self) -> &'static str {
        match self {
            Self::H264 => "H264",
            Self::H265 => "H265",
        }
    }

    pub(crate) const fn reo_name(self) -> &'static [u8; 4] {
        match self {
            Self::H264 => b"H264",
            Self::H265 => b"H265",
        }
    }
}

#[derive(Debug, Clone)]
pub struct VideoSource {
    pub(crate) codec: Codec,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) fps: u8,
    pub(crate) frame_interval: Duration,
    pub(crate) frames: Vec<EncodedFrame>,
}

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub(crate) data: Vec<u8>,
    pub(crate) is_keyframe: bool,
}

struct TrackDescription {
    track_id: u32,
    codec: Codec,
    width: u32,
    height: u32,
    fps: u8,
    frame_interval: Duration,
    nal_length_size: u8,
    parameter_sets: Vec<Vec<u8>>,
}

impl VideoSource {
    pub(crate) fn from_mp4(path: &Path) -> anyhow::Result<Self> {
        let mut reader = mp4::read_mp4(File::open(path)?).context("unable to read MP4")?;
        let description = describe_track(&reader)?;
        let sample_count = reader
            .sample_count(description.track_id)
            .context("unable to read video sample count")?;
        if sample_count == 0 {
            bail!("MP4 source has no video samples");
        }

        let mut frames = Vec::with_capacity(sample_count as usize);
        for sample_id in 1..=sample_count {
            let sample = reader
                .read_sample(description.track_id, sample_id)
                .with_context(|| format!("unable to read MP4 sample {sample_id}"))?
                .ok_or_else(|| anyhow!("MP4 sample {sample_id} is missing"))?;
            let mut data = if is_annex_b(&sample.bytes) {
                sample.bytes.to_vec()
            } else {
                length_prefixed_to_annex_b(&sample.bytes, description.nal_length_size)?
            };
            if sample.is_sync && !description.parameter_sets.is_empty() {
                let mut with_parameter_sets = Vec::with_capacity(
                    description
                        .parameter_sets
                        .iter()
                        .map(Vec::len)
                        .sum::<usize>()
                        + data.len()
                        + 16,
                );
                for parameter_set in &description.parameter_sets {
                    with_parameter_sets.extend_from_slice(&[0, 0, 0, 1]);
                    with_parameter_sets.extend_from_slice(parameter_set);
                }
                with_parameter_sets.append(&mut data);
                data = with_parameter_sets;
            }
            frames.push(EncodedFrame {
                data,
                is_keyframe: sample.is_sync,
            });
        }

        Ok(Self {
            codec: description.codec,
            width: description.width,
            height: description.height,
            fps: description.fps,
            frame_interval: description.frame_interval,
            frames,
        })
    }
}

fn describe_track<R: std::io::Read + std::io::Seek>(
    reader: &mp4::Mp4Reader<R>,
) -> anyhow::Result<TrackDescription> {
    for track in reader.tracks().values() {
        let codec = match track.media_type() {
            Ok(MediaType::H264) => Codec::H264,
            Ok(MediaType::H265) => Codec::H265,
            _ => continue,
        };
        let frame_rate = track.frame_rate();
        let fps = if frame_rate.is_finite() && frame_rate >= 1.0 {
            frame_rate.round().clamp(1.0, f64::from(u8::MAX)) as u8
        } else {
            15
        };
        let frame_interval = Duration::from_secs_f64(1.0 / f64::from(fps));
        let (nal_length_size, parameter_sets) = match codec {
            Codec::H264 => {
                let avcc = track
                    .trak
                    .mdia
                    .minf
                    .stbl
                    .stsd
                    .avc1
                    .as_ref()
                    .ok_or_else(|| anyhow!("H.264 MP4 track has no avc1 sample entry"))?;
                let nal_length_size = avcc
                    .avcc
                    .length_size_minus_one
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("H.264 NAL length size overflowed"))?;
                let parameter_sets = vec![
                    track.sequence_parameter_set()?.to_vec(),
                    track.picture_parameter_set()?.to_vec(),
                ];
                (nal_length_size, parameter_sets)
            }
            Codec::H265 => {
                let hevc = track
                    .trak
                    .mdia
                    .minf
                    .stbl
                    .stsd
                    .hev1
                    .as_ref()
                    .or(track.trak.mdia.minf.stbl.stsd.hvc1.as_ref())
                    .ok_or_else(|| anyhow!("H.265 MP4 track has no HEVC sample entry"))?;
                let configuration = hevc.hvcc.configuration()?;
                let vps = configuration
                    .vps
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("H.265 MP4 source has no VPS"))?;
                let sps = configuration
                    .sps
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("H.265 MP4 source has no SPS"))?;
                let pps = configuration
                    .pps
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("H.265 MP4 source has no PPS"))?;
                (configuration.nal_length_size, vec![vps, sps, pps])
            }
        };
        if !(1..=4).contains(&nal_length_size) {
            bail!("MP4 source uses unsupported NAL length size {nal_length_size}");
        }
        return Ok(TrackDescription {
            track_id: track.track_id(),
            codec,
            width: u32::from(track.width()),
            height: u32::from(track.height()),
            fps,
            frame_interval,
            nal_length_size,
            parameter_sets,
        });
    }
    bail!("MP4 source has no H.264 or H.265 video track")
}

fn is_annex_b(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1])
}

fn length_prefixed_to_annex_b(data: &[u8], length_size: u8) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::with_capacity(data.len() + data.len() / 16);
    let mut position = 0;
    while position < data.len() {
        let prefix_end = position + usize::from(length_size);
        if prefix_end > data.len() {
            bail!("truncated MP4 NAL length prefix");
        }
        let mut nal_len = 0usize;
        for byte in &data[position..prefix_end] {
            nal_len = (nal_len << 8) | usize::from(*byte);
        }
        position = prefix_end;
        let nal_end = position
            .checked_add(nal_len)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| anyhow!("MP4 NAL length exceeds sample size"))?;
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(&data[position..nal_end]);
        position = nal_end;
    }
    Ok(output)
}
