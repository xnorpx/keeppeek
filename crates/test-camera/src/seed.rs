use anyhow::{Context, bail};
use keeppeek::storage::{
    MediaFrame, RecordingCatalog, RecordingFrame, VideoCodec, VideoFrame,
    medium_term::MediumTermWriter,
};
use std::{fs::File, path::PathBuf, time::Duration};

pub struct RecordingSeedOptions {
    pub source: PathBuf,
    pub recordings: PathBuf,
    pub catalog: PathBuf,
    pub stream_id: String,
    pub duration: Duration,
    pub age: Duration,
}

struct SourceSample {
    duration: Duration,
    is_keyframe: bool,
    payload: Vec<u8>,
}

pub fn seed_recording(options: &RecordingSeedOptions) -> anyhow::Result<()> {
    let mut reader = mp4::read_mp4(File::open(&options.source)?)?;
    let (track_id, timescale, width, height, sps, pps, sample_count) = {
        let (&track_id, track) = reader
            .tracks()
            .iter()
            .find(|(_, track)| track.media_type().ok() == Some(mp4::MediaType::H264))
            .context("recording seed source has no H.264 video track")?;
        (
            track_id,
            track.timescale(),
            u32::from(track.width()),
            u32::from(track.height()),
            track.sequence_parameter_set()?.to_vec(),
            track.picture_parameter_set()?.to_vec(),
            track.sample_count(),
        )
    };
    if timescale == 0 || sample_count == 0 {
        bail!("recording seed source has no timed video samples");
    }

    let mut samples = Vec::with_capacity(sample_count as usize);
    let mut found_keyframe = false;
    for sample_id in 1..=sample_count {
        let Some(sample) = reader.read_sample(track_id, sample_id)? else {
            continue;
        };
        if !found_keyframe {
            if !sample.is_sync {
                continue;
            }
            found_keyframe = true;
        }
        let mut payload = sample.bytes.to_vec();
        if sample.is_sync {
            payload = parameterized_keyframe(&sps, &pps, &payload);
        }
        samples.push(SourceSample {
            duration: ticks_duration(u64::from(sample.duration), timescale),
            is_keyframe: sample.is_sync,
            payload,
        });
    }
    if !found_keyframe || samples.is_empty() {
        bail!("recording seed source has no keyframe-aligned samples");
    }

    let catalog = RecordingCatalog::open(&options.catalog)?;
    let started_at = std::time::Instant::now()
        .checked_sub(options.age)
        .context("recording seed age exceeds monotonic clock")?;
    let mut writer = MediumTermWriter::create_with_catalog(
        &options.recordings,
        &options.stream_id,
        started_at,
        64 * 1024,
        catalog.handle(),
    )?;
    let mut elapsed = Duration::ZERO;
    while elapsed < options.duration {
        for sample in &samples {
            if elapsed >= options.duration {
                break;
            }
            writer.append_one(RecordingFrame {
                received_at: started_at + elapsed,
                timestamp: Some(elapsed),
                frame: MediaFrame::Video(VideoFrame {
                    codec: VideoCodec::H264,
                    is_keyframe: sample.is_keyframe,
                    width,
                    height,
                    data: sample.payload.clone().into(),
                }),
            })?;
            elapsed = elapsed.saturating_add(sample.duration);
        }
    }
    writer.finalize()?;
    catalog.shutdown();
    Ok(())
}

fn parameterized_keyframe(sps: &[u8], pps: &[u8], sample: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + sps.len() + pps.len() + sample.len());
    append_avcc_nal(&mut payload, sps);
    append_avcc_nal(&mut payload, pps);
    payload.extend_from_slice(sample);
    payload
}

fn append_avcc_nal(payload: &mut Vec<u8>, nal: &[u8]) {
    let length = u32::try_from(nal.len()).expect("H.264 parameter set exceeds AVCC length");
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(nal);
}

fn ticks_duration(ticks: u64, timescale: u32) -> Duration {
    let nanos = u128::from(ticks).saturating_mul(1_000_000_000) / u128::from(timescale);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX).max(1))
}
