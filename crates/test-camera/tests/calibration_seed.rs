use keeppeek::storage::RecordingCatalog;
use std::{collections::BTreeSet, fs::File, time::Duration};
use test_camera::seed::{RecordingSeedOptions, seed_recording};

#[test]
fn calibration_seed_preserves_audio_and_contiguous_independent_periods() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/flash-tone-lead-200ms.mp4");
    let original = std::fs::read(&source)?;
    let catalog_path = root.path().join("catalog.db");
    seed_recording(&RecordingSeedOptions {
        calibration: true,
        source: source.clone(),
        recordings: root.path().join("media"),
        catalog: catalog_path.clone(),
        configuration: None,
        stream_id: "127.0.0.1/sub".into(),
        duration: Duration::from_secs(300),
        age: Duration::from_secs(120),
    })?;
    let catalog = RecordingCatalog::open(&catalog_path)?;
    let fragments = catalog
        .handle()
        .media_fragments_in_range("127.0.0.1/sub", 0, i64::MAX)?;
    assert_eq!(fragments.len(), 4);
    assert_eq!(
        fragments
            .iter()
            .map(|item| &item.recording_id)
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    assert_eq!(fragments[0].recording_id, fragments[1].recording_id);
    assert_ne!(fragments[1].recording_id, fragments[2].recording_id);
    assert_eq!(fragments[2].recording_id, fragments[3].recording_id);
    for pair in fragments.windows(2) {
        assert_eq!(
            pair[0].start_ms + i64::try_from(pair[0].duration_ms)?,
            pair[1].start_ms
        );
    }
    for fragment in [&fragments[0], &fragments[2]] {
        let reader = mp4::read_mp4(File::open(&fragment.path)?)?;
        assert_eq!(reader.moofs.len(), 2);
        assert_eq!(reader.tracks().len(), 2);
        assert!(reader.tracks().values().any(|track| track.track_type().ok()
            == Some(mp4::TrackType::Audio)
            && track.sample_count() > 0));
    }
    assert_eq!(std::fs::read(source)?, original);
    catalog.shutdown();
    Ok(())
}
