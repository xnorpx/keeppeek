use anyhow::{Context, Result, ensure};
use keeppeek::storage::{VideoCodec, medium_term::MediumTermWriter};
use std::{
    fs::File,
    path::Path,
    time::{Duration, Instant},
};

#[path = "fixtures/decoded_picture_ids.rs"]
mod decoded_picture_ids;

#[path = "fixtures/recording_frame.rs"]
mod recording_frame;

#[test]
fn replay_vfr_b_pictures_through_the_recorder_and_measure_presentation_timing() -> Result<()> {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exact_frame_step/vfr.mp4");
    let mut reader = mp4::read_mp4(File::open(&fixture)?)?;
    let input = &reader.tracks()[&1];
    ensure!(input.timescale() == 1000);
    let params = recording_frame::parameter_sets(input, VideoCodec::H264)?;
    let dimensions = (u32::from(input.width()), u32::from(input.height()));
    let root = std::env::temp_dir().join(format!("keeppeek-frame-step-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let start = Instant::now();
    let mut writer = MediumTermWriter::create(&root, "fixture", start, 8192)?;
    let mut source_ms = Vec::new();
    for ordinal in 1..=reader.sample_count(1)? {
        let timing = reader.tracks()[&1]
            .sample_timing(ordinal)?
            .context("missing input timing")?;
        let pts_ms = u64::try_from(timing.presentation_time)?;
        source_ms.push(pts_ms);
        let sample = reader
            .read_sample(1, ordinal)?
            .context("missing input sample")?;
        writer.append_one(recording_frame::frame(
            VideoCodec::H264,
            dimensions,
            sample,
            &params,
            start + Duration::from_millis(timing.decode_time),
            Duration::from_millis(pts_ms),
        ))?;
    }
    let path = writer.finalize()?;
    let recorded = mp4::read_mp4(File::open(&path)?)?;
    let track = &recorded.tracks()[&1];
    assert_eq!(track.sample_count(), 12);
    let mut changed = 0;
    for (index, expected) in source_ms.iter().enumerate() {
        let timing = track
            .sample_timing(u32::try_from(index + 1)?)?
            .context("missing recorded timing")?;
        if timing.presentation_time * 1000 != i128::from(*expected) * i128::from(track.timescale())
        {
            changed += 1;
        }
    }
    assert_eq!(
        changed, 10,
        "update the provenance qualification if the recorder preserves more timestamps"
    );
    let ids = decoded_picture_ids::decoded_picture_ids(&path);
    assert_eq!(ids, (0..12).collect::<Vec<_>>());
    tracing::subscriber::with_default(
        tracing_subscriber::fmt().with_test_writer().finish(),
        || {
            tracing::info!(
                pictures = ids.len(),
                changed_presentation_times = changed,
                "Recorded frame timing comparison"
            );
        },
    );
    drop(recorded);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
