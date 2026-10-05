use anyhow::{Context, bail};
use keeppeek::storage::{
    MediaFrame, RecordingCatalog, RecordingFrame, VideoCodec, VideoFrame,
    medium_term::MediumTermWriter,
};
use std::{
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

pub struct RecordingSeedOptions {
    pub source: PathBuf,
    pub recordings: PathBuf,
    pub catalog: PathBuf,
    pub configuration: Option<keeppeek::config::StorageToml>,
    pub stream_id: String,
    pub duration: Duration,
    pub age: Duration,
}

struct SourceSample {
    duration: Duration,
    is_keyframe: bool,
    payload: Vec<u8>,
}

struct SourceVideo {
    width: u32,
    height: u32,
    samples: Vec<SourceSample>,
}

fn source_video(path: &Path) -> anyhow::Result<SourceVideo> {
    let mut reader = mp4::read_mp4(File::open(path)?)?;
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
    Ok(SourceVideo {
        width,
        height,
        samples,
    })
}

pub fn seed_recording(options: &RecordingSeedOptions) -> anyhow::Result<()> {
    let SourceVideo {
        width,
        height,
        samples,
    } = source_video(&options.source)?;
    let started_at = std::time::Instant::now()
        .checked_sub(options.age)
        .context("recording seed age exceeds monotonic clock")?;
    let (catalog, mut writer) = seed_writer(options, started_at)?;
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

fn seed_writer(
    options: &RecordingSeedOptions,
    started_at: std::time::Instant,
) -> anyhow::Result<(RecordingCatalog, MediumTermWriter)> {
    if let Some(configuration) = &options.configuration {
        return configured_writer(configuration, &options.stream_id, started_at);
    }
    let catalog = RecordingCatalog::open(&options.catalog)?;
    let writer = MediumTermWriter::create_with_catalog(
        &options.recordings,
        &options.stream_id,
        started_at,
        64 * 1024,
        catalog.handle(),
    )?;
    Ok((catalog, writer))
}

fn configured_writer(
    configuration: &keeppeek::config::StorageToml,
    stream_id: &str,
    started_at: std::time::Instant,
) -> anyhow::Result<(RecordingCatalog, MediumTermWriter)> {
    use keeppeek::storage::{
        RecordingStreamIdentity, StorageConfig,
        catalog::locations::{Kind, Object},
        volumes::VolumeRole,
    };
    let mut storage = StorageConfig::from_toml(configuration);
    let binding = storage
        .metadata
        .as_ref()
        .context("seed metadata owner is missing")?;
    let catalog = RecordingCatalog::open_managed(
        &storage.recording_catalog_path,
        &binding.authority(),
        &binding.root_identity(),
    )?;
    storage.initialize_named_volumes(catalog.handle())?;
    let identity = RecordingStreamIdentity::legacy(stream_id);
    let id = uuid::Uuid::new_v4().to_string();
    let reservation = storage
        .volume_runtime
        .as_ref()
        .context("seed volume runtime is missing")?
        .reserve(
            VolumeRole::Active,
            &identity.source_id,
            &[],
            Object {
                kind: Kind::Recording,
                id: id.clone(),
            },
            65_536,
        )?
        .context("seed placement is unavailable")?;
    let writer = MediumTermWriter::create_with_reservation(
        reservation,
        id,
        identity,
        started_at,
        64 * 1024,
        catalog.handle(),
    )?;
    Ok((catalog, writer))
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
