use super::*;
use crate::storage::archive::target::Work;

fn recording(path: &Path, times: &[Option<i64>]) -> Vec<CatalogMediaFragment> {
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap()],
        timescale: 1_000,
    };
    let track = mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: 1_000,
        language: "und".into(),
        media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
            width: 320,
            height: 240,
            seq_param_set: vec![0x67, 0x42, 0, 0x1e, 0xe9, 1, 0x40, 0x7b, 0x20],
            pic_param_set: vec![0x68, 0xce, 6, 0xe2],
        }),
    };
    let mut writer = mp4::FragmentedMp4Writer::write_start(
        std::fs::File::create(path).unwrap(),
        &config,
        &[track],
    )
    .unwrap();
    let mut fragments = Vec::new();
    for (index, &stamp) in times.iter().enumerate() {
        let sync = index % 3 == 0;
        let sample = mp4::Mp4Sample {
            start_time: index as u64 * 40,
            duration: 40,
            rendering_offset: 0,
            is_sync: sync,
            bytes: bytes::Bytes::from(vec![
                0,
                0,
                0,
                2,
                if sync { 0x65 } else { 0x41 },
                index as u8,
            ]),
        };
        let packet = stamp.map(|timestamp| mp4::SourceRtpPacket {
            session_id: [1; 16],
            clock_rate: 1_000.try_into().unwrap(),
            timestamp,
            discontinuity: index == 0,
        });
        writer
            .write_sample_with_source_timing(1, 1, sample, packet)
            .unwrap();
        if index % 3 == 2 {
            fragments.push(writer.flush_fragment().unwrap().unwrap());
        }
    }
    writer.write_end().unwrap();
    let init = fragments[0].range.offset;
    fragments
        .into_iter()
        .enumerate()
        .map(|(i, f)| CatalogMediaFragment {
            recording_id: "first".into(),
            recording_started_at_ms: 10_000,
            path: path.to_str().unwrap().into(),
            init_offset: 0,
            init_len: init,
            sequence: u64::from(f.sequence_number),
            start_ms: 10_000 + i as i64 * 120,
            duration_ms: 120,
            byte_offset: f.range.offset,
            byte_len: f.range.size,
            archive: None,
        })
        .collect()
}

fn work() -> Work<'static> {
    Work {
        deadline: Instant::now() + Duration::from_secs(2),
        cancelled: &|| false,
    }
}

#[test]
fn returns_the_source_adjacent_sample_and_reverses_across_a_fragment() {
    let dir = tempfile::tempdir().unwrap();
    let fragments = recording(
        &dir.path().join("recording.mp4"),
        &[Some(0), Some(80), Some(40), Some(120), Some(180), Some(160)],
    );
    let request = proto::StepStoredFrame {
        generation: 1,
        step_id: "step".into(),
        stored_media_id: "playback".into(),
        ..Default::default()
    };
    let next = target_window(&fragments, 10_000_000_000, None, true, &work()).unwrap();
    assert_eq!(next.index.pictures[next.target].ordinal, 3);
    let decoded = encode(next, &request, &work()).unwrap();
    assert_eq!(decoded.result.source_ticks, 40);
    assert_eq!(decoded.result.target_presentation_rank, 1);
    assert_eq!(decoded.result.sample_count, 3);
    let reverse = target_window(
        &fragments,
        decoded.selected.stored_ns,
        Some(&decoded.selected),
        false,
        &work(),
    )
    .unwrap();
    assert_eq!(reverse.index.pictures[reverse.target].ordinal, 1);
    let crossing = target_window(&fragments, 10_120_000_000, None, false, &work()).unwrap();
    assert_eq!(crossing.fragment.sequence, fragments[0].sequence);
    assert_eq!(crossing.index.pictures[crossing.target].ordinal, 2);
    let mut changed = fragments.clone();
    changed[0].recording_id = "replacement".into();
    assert!(matches!(
        target_window(
            &changed,
            decoded.selected.stored_ns,
            Some(&decoded.selected),
            true,
            &work()
        ),
        Err(Unavailable::Stale)
    ));
    changed[1].start_ms += 1_000;
    assert!(matches!(
        target_window(&changed, 10_040_000_000, None, true, &work()),
        Err(Unavailable::Gap)
    ));
}

#[test]
fn never_uses_storage_decode_order_to_guess_an_unknown_picture() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recording.mp4");
    for (times, reason) in [
        ([Some(0), Some(40), Some(40)], Unavailable::Ambiguous),
        ([Some(0), None, Some(80)], Unavailable::Timing),
    ] {
        let fragments = recording(&path, &times);
        assert_eq!(
            target_window(&fragments, 10_000_000_000, None, true, &work()).err(),
            Some(reason)
        );
    }
}
