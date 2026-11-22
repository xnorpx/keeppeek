use super::*;
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

struct Media {
    initialization: Vec<u8>,
    fragments: Vec<Vec<u8>>,
    origin_ms: i64,
}

struct Decoded {
    video: Vec<mp4::Mp4Sample>,
    audio: Vec<mp4::Mp4Sample>,
}

impl Media {
    fn period(&self, index: usize) -> Period<'_> {
        Period {
            initialization: &self.initialization,
            fragment: &self.fragments[index],
            recording_started_at_ms: self.origin_ms,
        }
    }

    fn digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(&self.initialization);
        for fragment in &self.fragments {
            digest.update(fragment);
        }
        digest.finalize().into()
    }
}

fn fixture(rate: Option<u32>, audio: &[Vec<(u64, i32, u8)>]) -> Media {
    let mut tracks = vec![video_configuration()];
    if let Some(rate) = rate {
        tracks.push(audio_configuration(rate));
    }
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap()],
        timescale: 1_000,
    };
    let mut writer =
        mp4::FragmentedMp4Writer::write_start(Cursor::new(Vec::new()), &config, &tracks).unwrap();
    let initialization = writer.initialization();
    let mut fragments = Vec::new();
    for (index, packets) in audio.iter().enumerate() {
        writer
            .write_sample(1, video_sample(u64::try_from(index).unwrap() * 90_000))
            .unwrap();
        if let Some(rate) = rate {
            for &(start_ms, composition, marker) in packets {
                writer
                    .write_sample(2, audio_sample(rate, start_ms, composition, marker))
                    .unwrap();
            }
        }
        fragments.push(writer.flush_fragment().unwrap().unwrap().range);
    }
    let bytes = writer.into_writer().into_inner();
    Media {
        initialization: slice(&bytes, initialization),
        fragments: fragments
            .into_iter()
            .map(|range| slice(&bytes, range))
            .collect(),
        origin_ms: 10_000,
    }
}

fn video_configuration() -> mp4::TrackConfig {
    mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: 90_000,
        language: "und".into(),
        media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
            width: 320,
            height: 240,
            seq_param_set: vec![0x67, 0x42, 0, 0x1e, 0xe9, 1, 0x40, 0x7b, 0x20],
            pic_param_set: vec![0x68, 0xce, 6, 0xe2],
        }),
    }
}

fn audio_configuration(rate: u32) -> mp4::TrackConfig {
    mp4::TrackConfig {
        track_type: mp4::TrackType::Audio,
        timescale: rate,
        language: "und".into(),
        media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
            bitrate: 64_000,
            profile: mp4::AudioObjectType::AacLowComplexity,
            freq_index: match rate {
                16_000 => mp4::SampleFreqIndex::Freq16000,
                48_000 => mp4::SampleFreqIndex::Freq48000,
                _ => panic!("unsupported fixture rate"),
            },
            chan_conf: mp4::ChannelConfig::Mono,
        }),
    }
}

fn video_sample(start_time: u64) -> mp4::Mp4Sample {
    // ponytail: Structural markers qualify remux timing; decoded calibration is separate.
    mp4::Mp4Sample {
        start_time,
        duration: 90_000,
        rendering_offset: 450,
        is_sync: true,
        bytes: bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65]),
    }
}

fn audio_sample(rate: u32, start_ms: u64, composition: i32, marker: u8) -> mp4::Mp4Sample {
    mp4::Mp4Sample {
        start_time: start_ms * u64::from(rate) / 1_000,
        duration: rate / 50,
        rendering_offset: composition,
        is_sync: true,
        bytes: vec![marker, 0x55, 0xaa].into(),
    }
}

fn slice(bytes: &[u8], range: mp4::Mp4ByteRange) -> Vec<u8> {
    bytes[usize::try_from(range.offset).unwrap()
        ..usize::try_from(range.offset + range.size).unwrap()]
        .to_vec()
}

fn options(offset_ms: i32, start_ms: i64, end_ms: i64) -> Options {
    Options {
        audio_offset_ms: offset_ms,
        playback_origin_ms: 10_000,
        window_start_ms: start_ms,
        window_end_ms: end_ms,
    }
}

fn work() -> Work<'static> {
    Work {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: &|| false,
    }
}

fn decode(output: &RemuxedFragment) -> Decoded {
    let mut bytes = output.initialization.clone();
    bytes.extend_from_slice(&output.fragment);
    let mut reader = mp4::Mp4Reader::read_header(
        Cursor::new(bytes.clone()),
        u64::try_from(bytes.len()).unwrap(),
    )
    .unwrap();
    assert_eq!(reader.moofs.len(), 1);
    assert_eq!(reader.tracks().len(), 2);
    let mut result = Decoded {
        video: Vec::new(),
        audio: Vec::new(),
    };
    for kind in [mp4::TrackType::Video, mp4::TrackType::Audio] {
        let (&id, track) = reader
            .tracks()
            .iter()
            .find(|(_, track)| track.track_type().unwrap() == kind)
            .unwrap();
        let count = track.sample_count();
        let samples = (1..=count)
            .map(|index| reader.read_sample(id, index).unwrap().unwrap())
            .collect();
        match kind {
            mp4::TrackType::Video => result.video = samples,
            mp4::TrackType::Audio => result.audio = samples,
            mp4::TrackType::Subtitle => panic!("unexpected subtitle"),
        }
    }
    result
}

fn audio_timeline(output: &Decoded) -> Vec<(u64, i32, u32, u8)> {
    output
        .audio
        .iter()
        .map(|sample| {
            (
                sample.start_time,
                sample.rendering_offset,
                sample.duration,
                sample.bytes[0],
            )
        })
        .collect()
}

fn reordered_pts_fixture(audio_composition: bool) -> Media {
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap()],
        timescale: 1_000,
    };
    let mut writer = mp4::FragmentedMp4Writer::write_start(
        Cursor::new(Vec::new()),
        &config,
        &[video_configuration(), audio_configuration(48_000)],
    )
    .unwrap();
    let initialization = writer.initialization();
    for (start, composition) in [(0, 9_000), (3_600, -1_800)] {
        let mut sample = video_sample(start);
        sample.duration = 3_600;
        sample.rendering_offset = composition;
        writer.write_sample(1, sample).unwrap();
    }
    for (start, composition, marker) in [(0, 4_800, 1), (20, -480, 2)] {
        let composition = if audio_composition { composition } else { 0 };
        writer
            .write_sample(2, audio_sample(48_000, start, composition, marker))
            .unwrap();
    }
    let fragment = writer.write_end().unwrap().unwrap().range;
    let bytes = writer.into_writer().into_inner();
    Media {
        initialization: slice(&bytes, initialization),
        fragments: vec![slice(&bytes, fragment)],
        origin_ms: 10_000,
    }
}

#[test]
fn observations_use_earliest_original_presentation_across_decode_order() {
    let media = reordered_pts_fixture(true);
    let original = media.digest();
    let output = remux(
        media.period(0),
        &[media.period(0)],
        options(0, 10_000, 11_000),
        &work(),
    )
    .unwrap();
    assert_eq!(output.evidence.first_video_pts_us, Some(20_000));
    assert_eq!(output.evidence.first_audio_pts_us, Some(10_000));
    assert_eq!(output.evidence.stored_skew_us(), Some(-10_000));
    assert!(output.evidence.audio_present);
    let parsed = decode(&output);
    assert_eq!(
        parsed
            .video
            .iter()
            .map(|sample| (sample.start_time, sample.rendering_offset))
            .collect::<Vec<_>>(),
        vec![(0, 9_000), (3_600, -1_800)]
    );
    assert_eq!(
        audio_timeline(&parsed),
        vec![(0, 4_800, 960, 1), (960, -480, 960, 2)]
    );
    assert_eq!(media.digest(), original);
}

#[test]
fn delayed_audio_uses_previous_fragment_without_moving_video() {
    let media = fixture(
        Some(48_000),
        &[
            vec![(900, 0, 1), (980, 0, 2)],
            vec![(1_000, 0, 3), (1_080, 0, 4)],
        ],
    );
    let original = media.digest();
    let output = remux(
        media.period(1),
        &[media.period(0), media.period(1)],
        options(200, 11_000, 12_000),
        &work(),
    )
    .unwrap();
    let parsed = decode(&output);
    assert_eq!(parsed.video[0].start_time, 90_000);
    assert_eq!(parsed.video[0].rendering_offset, 450);
    assert_eq!(parsed.video[0].duration, 90_000);
    assert_eq!(parsed.video[0].bytes.as_ref(), &[0, 0, 0, 1, 0x65]);
    assert_eq!(
        audio_timeline(&parsed),
        vec![
            (52_800, 0, 3_840, 1),
            (56_640, 0, 960, 2),
            (57_600, 0, 3_840, 3),
            (61_440, 0, 960, 4)
        ]
    );
    assert_eq!(media.digest(), original);
}

#[test]
fn advanced_audio_uses_next_fragment_and_half_open_windows_do_not_duplicate() {
    let media = fixture(
        Some(16_000),
        &[
            vec![(900, 0, 1), (980, 0, 2)],
            vec![(1_000, 0, 3), (1_080, 0, 4)],
        ],
    );
    let context = [media.period(0), media.period(1), media.period(1)];
    let output = remux(
        media.period(0),
        &context,
        options(-200, 10_000, 11_000),
        &work(),
    )
    .unwrap();
    assert_eq!(
        audio_timeline(&decode(&output)),
        vec![
            (11_200, 0, 1_280, 1),
            (12_480, 0, 320, 2),
            (12_800, 0, 1_280, 3),
            (14_080, 0, 320, 4)
        ]
    );
    let following = remux(
        media.period(1),
        &context,
        options(-200, 11_000, 12_000),
        &work(),
    )
    .unwrap();
    assert!(decode(&following).audio.is_empty());
    assert_eq!(decode(&following).video[0].start_time, 90_000);
    assert_eq!(output.initialization, following.initialization);
}

#[test]
fn adjacent_output_windows_select_an_exact_boundary_packet_once() {
    let media = fixture(
        Some(48_000),
        &[vec![(800, 0, 1), (980, 0, 2)], vec![(1_000, 0, 3)]],
    );
    let context = [media.period(0), media.period(1)];
    let first = remux(
        media.period(0),
        &context,
        options(200, 10_000, 11_000),
        &work(),
    )
    .unwrap();
    let next = remux(
        media.period(1),
        &context,
        options(200, 11_000, 12_000),
        &work(),
    )
    .unwrap();
    assert!(decode(&first).audio.is_empty());
    assert_eq!(
        audio_timeline(&decode(&next)),
        vec![
            (48_000, 0, 8_640, 1),
            (56_640, 0, 960, 2),
            (57_600, 0, 960, 3)
        ]
    );
}

#[test]
fn empty_audio_period_keeps_initialization_and_reports_no_measured_difference() {
    let media = fixture(Some(48_000), &[Vec::new(), vec![(1_000, 0, 1)]]);
    let empty = remux(media.period(0), &[], options(-200, 10_000, 11_000), &work()).unwrap();
    assert!(decode(&empty).audio.is_empty());
    // A declared AAC track without encoded samples is not observed audio.
    assert!(!empty.evidence.audio_present);
    assert_eq!(empty.evidence.first_audio_pts_us, None);
    assert_eq!(empty.evidence.stored_skew_us(), None);
    let next = remux(
        media.period(1),
        &[media.period(1)],
        options(200, 11_000, 12_000),
        &work(),
    )
    .unwrap();
    assert_eq!(empty.initialization, next.initialization);
}

#[test]
fn common_origin_and_signed_composition_survive_recording_changes_and_seeks() {
    let first = fixture(Some(48_000), &[vec![(50, 0, 1)]]);
    let mut next = fixture(Some(48_000), &[vec![(50, 0, 2)]]);
    next.origin_ms = 14_000;
    let originals = (first.digest(), next.digest());
    let left = remux(
        first.period(0),
        &[first.period(0)],
        options(200, 10_000, 11_000),
        &work(),
    )
    .unwrap();
    let right = remux(
        next.period(0),
        &[next.period(0)],
        options(200, 14_000, 15_000),
        &work(),
    )
    .unwrap();
    assert_eq!(audio_timeline(&decode(&left)), vec![(12_000, 0, 960, 1)]);
    assert_eq!(audio_timeline(&decode(&right)), vec![(204_000, 0, 960, 2)]);
    assert_eq!(decode(&right).video[0].start_time, 360_000);
    assert_eq!(right.evidence.first_video_pts_us, Some(4_005_000));
    assert_eq!(right.evidence.first_audio_pts_us, Some(4_050_000));
    assert_eq!(right.evidence.stored_skew_us(), Some(45_000));
    let seek = remux(
        next.period(0),
        &[next.period(0)],
        options(200, 14_000, 15_000),
        &work(),
    )
    .unwrap();
    assert_eq!(right.fragment, seek.fragment);
    assert_eq!((first.digest(), next.digest()), originals);
}

#[test]
fn corrected_audio_rejects_composition_offsets_without_changing_source() {
    for composition in [-480, 480] {
        let media = fixture(Some(48_000), &[vec![(100, composition, 1)]]);
        let original = media.digest();
        for offset in [-50, 50] {
            assert!(
                remux(
                    media.period(0),
                    &[media.period(0)],
                    options(offset, 10_000, 11_000),
                    &work()
                )
                .is_err()
            );
        }
        assert_eq!(media.digest(), original);
    }
}

#[test]
fn corrected_audio_preserves_positive_and_negative_video_composition() {
    let media = reordered_pts_fixture(false);
    let original = media.digest();
    let output = remux(
        media.period(0),
        &[media.period(0)],
        options(100, 10_000, 11_000),
        &work(),
    )
    .unwrap();
    let parsed = decode(&output);
    assert_eq!(
        parsed
            .video
            .iter()
            .map(|sample| (sample.start_time, sample.rendering_offset, sample.duration))
            .collect::<Vec<_>>(),
        vec![(0, 9_000, 3_600), (3_600, -1_800, 3_600)]
    );
    assert_eq!(
        audio_timeline(&parsed),
        vec![(4_800, 0, 960, 1), (5_760, 0, 960, 2)]
    );
    assert_eq!(media.digest(), original);
}

#[test]
fn negative_audio_samples_are_clipped_but_video_fragment_is_valid() {
    let media = fixture(Some(48_000), &[vec![(50, 0, 1)]]);
    let output = remux(
        media.period(0),
        &[media.period(0)],
        options(-200, 10_000, 11_000),
        &work(),
    )
    .unwrap();
    assert!(decode(&output).audio.is_empty());
    assert_eq!(output.evidence.clipped_audio_samples, 1);
    assert_eq!(decode(&output).video.len(), 1);
}

#[test]
fn missing_audio_and_incompatible_neighbor_codecs_fail_closed() {
    let silent = fixture(None, &[Vec::new()]);
    assert!(remux(silent.period(0), &[], options(100, 10_000, 11_000), &work()).is_err());
    let media = fixture(Some(48_000), &[vec![(100, 0, 1)]]);
    let incompatible = fixture(Some(16_000), &[vec![(100, 0, 1)]]);
    assert!(
        remux(
            media.period(0),
            &[incompatible.period(0)],
            options(100, 10_000, 11_000),
            &work()
        )
        .is_err()
    );
}

#[test]
fn disjoint_audio_runs_and_conflicting_duplicate_packets_fail_closed() {
    let media = fixture(Some(48_000), &[vec![(100, 0, 1)], vec![(1_000, 0, 2)]]);
    let context = [media.period(0), media.period(1)];
    assert!(
        remux(
            media.period(0),
            &context,
            options(100, 10_000, 12_000),
            &work()
        )
        .is_err()
    );
    let conflicting = fixture(Some(48_000), &[vec![(100, 0, 99)]]);
    assert!(
        remux(
            media.period(0),
            &[media.period(0), conflicting.period(0)],
            options(100, 10_000, 11_000),
            &work()
        )
        .is_err()
    );
    let output = remux(
        media.period(0),
        &[media.period(0)],
        options(100, 10_000, 11_000),
        &work(),
    )
    .unwrap();
    assert_eq!(audio_timeline(&decode(&output)), vec![(9_600, 0, 960, 1)]);
}

#[test]
fn malformed_period_and_invalid_window_do_not_produce_output() {
    let media = fixture(Some(48_000), &[vec![(100, 0, 1)]]);
    let truncated = Period {
        fragment: &media.fragments[0][..15],
        ..media.period(0)
    };
    assert!(remux(truncated, &[], options(100, 10_000, 11_000), &work()).is_err());
    assert!(remux(media.period(0), &[], options(100, 11_000, 10_000), &work()).is_err());
    let before_origin = Options {
        playback_origin_ms: 11_000,
        ..options(100, 10_000, 11_000)
    };
    assert!(remux(media.period(0), &[], before_origin, &work()).is_err());
}

#[test]
fn cancellation_deadline_and_resource_bounds_reject_before_output() {
    let media = fixture(Some(48_000), &[vec![(100, 0, 1)]]);
    let original = media.digest();
    let calls = Cell::new(0);
    let cancelled = || {
        calls.set(calls.get() + 1);
        calls.get() > 5
    };
    let cancelled_work = Work {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: &cancelled,
    };
    assert!(
        remux(
            media.period(0),
            &[media.period(0)],
            options(100, 10_000, 11_000),
            &cancelled_work
        )
        .is_err()
    );
    let expired = Work {
        deadline: Instant::now(),
        cancelled: &|| false,
    };
    assert!(remux(media.period(0), &[], options(100, 10_000, 11_000), &expired).is_err());
    let contexts = vec![media.period(0); CONTEXT_PERIODS_MAX + 1];
    assert!(
        remux(
            media.period(0),
            &contexts,
            options(100, 10_000, 11_000),
            &work()
        )
        .is_err()
    );
    let oversized = vec![0; INPUT_BYTES_MAX + 1];
    let period = Period {
        fragment: &oversized,
        ..media.period(0)
    };
    assert!(remux(period, &[], options(100, 10_000, 11_000), &work()).is_err());
    assert_eq!(media.digest(), original);
}

#[test]
fn excessive_declared_sample_count_is_rejected_before_the_mp4_reader() {
    let media = fixture(Some(48_000), &[vec![(100, 0, 1)]]);
    let mut fragment = media.fragments[0].clone();
    let trun = fragment
        .windows(4)
        .position(|kind| kind == b"trun")
        .unwrap();
    fragment[trun + 8..trun + 12].copy_from_slice(&65_537_u32.to_be_bytes());
    let malformed = Period {
        fragment: &fragment,
        ..media.period(0)
    };
    assert!(remux(malformed, &[], options(100, 10_000, 11_000), &work()).is_err());
}
