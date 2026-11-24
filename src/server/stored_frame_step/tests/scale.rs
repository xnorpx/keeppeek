//! Cold catalog setup is outside the timed, repeated local disk queries.
use super::*;
use crate::storage::{CatalogFragment, CatalogRecording};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reads {
    calls: u64,
    bytes: u64,
}

thread_local! {
    static READS: Cell<Option<Reads>> = const { Cell::new(None) };
}

pub(in crate::server::stored_frame_step) fn observe_range(bytes: u64) {
    READS.with(|cell| {
        cell.set(cell.get().map(|counts| Reads {
            calls: counts.calls + 1,
            bytes: counts.bytes + bytes,
        }));
    });
    });
}

fn measured<T>(run: impl FnOnce() -> T) -> (T, Reads) {
    READS.set(Some(Reads::default()));
    let result = run();
    (result, READS.replace(None).unwrap())
}

fn add_neighbors(catalog: &RecordingCatalogHandle, root: &Path, parts: &[CatalogMediaFragment]) {
    let template = &parts[0];
    for number in 1..127 {
        let source = format!("scale-{number:03}");
        let path = root.join(format!("{source}.mp4"));
        std::fs::copy(&template.path, &path).unwrap();
        catalog
            .upsert_recording(CatalogRecording {
                id: source.clone(),
                stream_id: format!("{source}/sub"),
                source_id: Some(source.clone()),
                logical_stream_id: Some("sub".into()),
                started_at_ms: template.recording_started_at_ms,
                ended_at_ms: Some(10_240),
                path: path.to_str().unwrap().into(),
                init_offset: template.init_offset,
                init_len: template.init_len,
                finalized: true,
            })
            .unwrap();
        for part in parts {
            catalog
                .insert_fragment(CatalogFragment {
                    recording_id: source.clone(),
                    sequence: part.sequence,
                    start_ms: part.start_ms,
                    duration_ms: part.duration_ms,
                    byte_offset: part.byte_offset,
                    byte_len: part.byte_len,
                    random_access: true,
                })
                .unwrap();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Cost {
    fragments: usize,
    selection: Reads,
    payload: Reads,
    decode_samples: u32,
}

fn series(catalog: &RecordingCatalogHandle, sources: usize, forward: bool) -> Cost {
    const TRIALS: usize = 30;
    let anchor_ms = if forward { 10_000 } else { 10_120 };
    let mut query_us = Vec::new();
    let mut selection_us = Vec::new();
    let mut delivery_us = Vec::new();
    let mut previous = None;
    for _ in 0..TRIALS {
        let begin = Instant::now();
        let limits = work();
        let (_, fragments, _lease) = catalog
            .frame_step_window("front-door/sub", anchor_ms, limits.deadline)
            .unwrap();
        query_us.push(begin.elapsed().as_micros());
        assert_eq!(fragments.len(), 2);
        assert!(fragments.iter().all(|part| part.recording_id == "first"));

        let (batch, selection) = measured(|| {
            let window = target_window(
                &fragments,
                i128::from(anchor_ms) * 1_000_000,
                None,
                forward,
                &limits,
            )
            .unwrap();
            let request = proto::StepStoredFrame {
                step_id: "scale".into(),
                stored_media_id: "recording".into(),
                generation: 1,
                ..Default::default()
            };
            plan(window, &request, &limits).unwrap()
        });
        selection_us.push(begin.elapsed().as_micros());
        assert_eq!(batch.result.source_ticks, if forward { 40 } else { 80 });
        assert_eq!(batch.result.sample_count, 3);
        let start_delivery = Instant::now();
        let (messages, payload) = measured(|| batch.messages(&limits).unwrap());
        delivery_us.push(start_delivery.elapsed().as_micros());
        assert_eq!(messages.len(), 3);
        assert_eq!(payload.bytes, u64::from(batch.result.encoded_bytes));
        let cost = Cost {
            fragments: fragments.len(),
            selection,
            payload,
            decode_samples: batch.result.sample_count,
        };
        if let Some(prior) = previous.replace(cost) {
            assert_eq!(Some(prior), previous);
        }
    }
    query_us.sort_unstable();
    selection_us.sort_unstable();
    delivery_us.sort_unstable();
    let cost = previous.unwrap();
    // Nearest-rank p50 and p95 for exactly 30 observations, in microseconds.
    println!(
        "frame-window sources={sources} forward={forward} trials={TRIALS} catalog_commands_per_trial=1 catalog_us_p50_p95={}/{} first_response_work_us_p50_p95={}/{} payload_us_p50_p95={}/{} cost={cost:?}",
        query_us[14],
        query_us[28],
        selection_us[14],
        selection_us[28],
        delivery_us[14],
        delivery_us[28]
    );
    cost
}

#[test]
#[ignore = "explicit single-worker, 120-step disk and catalog qualification"]
fn frame_window_scale_one_and_127_sources() {
    let root = RecordingDirectory::new();
    let (state, catalog) = routed_fixture(&root.0);
    let handle = state.catalog.as_ref().unwrap();
    let base_forward = series(handle, 1, true);
    let base_reverse = series(handle, 1, false);
    let (_, parts, lease) = handle
        .frame_step_window("front-door/sub", 10_000, work().deadline)
        .unwrap();
    drop(lease);
    add_neighbors(handle, &root.0, &parts);
    assert_eq!(series(handle, 127, true), base_forward);
    assert_eq!(series(handle, 127, false), base_reverse);
    drop(state);
    catalog.shutdown();
}

#[test]
fn full_4k_avc_and_hevc_gops_fit_the_source_selection_and_payload_limits() {
    for codec in ["h264", "h265"] {
        let filename = format!("cc-4k-3840x2160-{codec}.mp4");
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata")
            .join(filename);
        let mut input = mp4::read_mp4(std::fs::File::open(source).unwrap()).unwrap();
        let video = &input.tracks()[&1];
        assert_eq!(
            (video.width(), video.height(), video.sample_count()),
            (3840, 2160, 15)
        );
        let rate = video.timescale();
        let track = mp4::TrackConfig {
            track_type: mp4::TrackType::Video,
            timescale: rate,
            language: "und".into(),
            media_conf: video.media_config_for_description(1).unwrap(),
        };
        let directory = RecordingDirectory::new();
        let path = directory.0.join("4k.mp4");
        let mut writer = mp4::FragmentedMp4Writer::write_start(
            std::fs::File::create(&path).unwrap(),
            &mp4::Mp4Config {
                major_brand: "iso6".parse().unwrap(),
                minor_version: 1,
                compatible_brands: vec!["iso6".parse().unwrap()],
                timescale: rate,
            },
            &[track],
        )
        .unwrap();
        let mut encoded_bytes = 0;
        let mut timestamps = Vec::new();
        for ordinal in 1..=15 {
            let timing = input.tracks()[&1].sample_timing(ordinal).unwrap().unwrap();
            let timestamp = i64::try_from(timing.presentation_time).unwrap();
            let mut sample = input.read_sample(1, ordinal).unwrap().unwrap();
            encoded_bytes += sample.bytes.len();
            timestamps.push((timestamp, ordinal));
            sample.rendering_offset = 0;
            writer
                .write_sample_with_source_timing(
                    1,
                    1,
                    sample,
                    Some(mp4::SourceRtpPacket {
                        session_id: [1; 16],
                        epoch_id: [2; 16],
                        clock_rate: rate.try_into().unwrap(),
                        timestamp,
                    }),
                )
                .unwrap();
        }
        let fragment = writer.flush_fragment().unwrap().unwrap();
        writer.write_end().unwrap();
        let original_digest = Sha256::digest(std::fs::read(&path).unwrap());
        let entries = [CatalogMediaFragment {
            recording_id: codec.into(),
            recording_started_at_ms: 10_000,
            path: path.to_str().unwrap().into(),
            init_offset: 0,
            init_len: fragment.range.offset,
            sequence: u64::from(fragment.sequence_number),
            start_ms: 10_000,
            duration_ms: 1_000,
            byte_offset: fragment.range.offset,
            byte_len: fragment.range.size,
            archive: None,
        }];
        let request = proto::StepStoredFrame {
            step_id: "4k".into(),
            generation: 1,
            stored_media_id: "stored".into(),
            ..Default::default()
        };
        let mut selected = None;
        timestamps.sort_unstable();
        assert_eq!(timestamps[0].1, 1);
        for (forward, at) in [(true, 1), (false, 0)] {
            let anchor = selected
                .as_ref()
                .map_or(10_000_000_000, |sample: &Selected| sample.stored_ns);
            let window =
                target_window(&entries, anchor, selected.as_ref(), forward, &work()).unwrap();
            let output = plan(window, &request, &work()).unwrap();
            assert_eq!(
                (output.result.source_ticks, output.selected.ordinal),
                timestamps[at]
            );
            assert_eq!(output.result.sample_count, 15);
            assert_eq!(output.result.encoded_bytes as usize, encoded_bytes);
            let parts = output.messages(&work()).unwrap();
            assert!(parts.len() >= 15);
            println!(
                "4K {codec} forward={forward} samples=15 encoded_bytes={encoded_bytes} chunks={}",
                parts.len()
            );
            selected = Some(output.selected);
        }
        assert_eq!(
            Sha256::digest(std::fs::read(&path).unwrap()),
            original_digest
        );
    }
}
