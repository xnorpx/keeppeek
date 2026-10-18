use super::*;
use sha2::{Digest, Sha256};
use std::cell::Cell;

mod export_context;

struct Fixture {
    root: PathBuf,
    fragments: Vec<CatalogMediaFragment>,
    originals: Vec<(PathBuf, [u8; 32])>,
}

struct Output {
    bytes: Vec<u8>,
    video: Vec<mp4::Mp4Sample>,
    audio: Vec<mp4::Mp4Sample>,
    audio_timescale: Option<u32>,
}

impl Fixture {
    fn new(audio_rate: Option<u32>, first_audio_ms: u64, audio_cts: i32) -> Self {
        Self::with_audio_bitrate(audio_rate, first_audio_ms, audio_cts, 64_000)
    }

    fn with_audio_bitrate(
        audio_rate: Option<u32>,
        first_audio_ms: u64,
        audio_cts: i32,
        audio_bitrate: u32,
    ) -> Self {
        let root = std::env::temp_dir().join(format!("audio-sync-remux-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut fragments = Vec::new();
        let mut originals = Vec::new();
        for recording in 0..2 {
            let path = root.join(format!("source-{recording}.mp4"));
            fragments.extend(write_source(
                &path,
                recording,
                audio_rate,
                first_audio_ms,
                audio_cts,
                audio_bitrate,
                1_100,
            ));
            originals.push((
                path.clone(),
                Sha256::digest(std::fs::read(path).unwrap()).into(),
            ));
        }
        Self {
            root,
            fragments,
            originals,
        }
    }

    fn export(&self, name: &str, offset_ms: i32) -> Output {
        let path = self.root.join(name);
        let artifact = export_fragment_ranges_with_audio_offset(
            &self.fragments,
            14_000,
            &path,
            offset_ms,
            || false,
            |_| {},
        )
        .unwrap();
        let output = read_output(&path);
        assert_eq!(artifact.bytes, u64::try_from(output.bytes.len()).unwrap());
        self.assert_originals();
        output
    }

    fn assert_originals(&self) {
        for (path, expected) in &self.originals {
            let observed: [u8; 32] = Sha256::digest(std::fs::read(path).unwrap()).into();
            assert_eq!(&observed, expected, "source evidence changed");
        }
    }

    fn assert_no_partial(&self, destination: &Path) {
        assert!(!destination.exists());
        assert!(std::fs::read_dir(&self.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".active")
        }));
        self.assert_originals();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn video_configuration() -> (mp4::TrackConfig, bytes::Bytes) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/test-camera/testdata/cc-4k-640x360-h264.mp4");
    let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
    let (&id, track) = reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().unwrap() == mp4::TrackType::Video)
        .unwrap();
    let config = mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: 90_000,
        language: "und".into(),
        media_conf: track.media_config_for_description(1).unwrap(),
    };
    let sample = (1..=track.sample_count())
        .find_map(|index| {
            let sample = reader.read_sample(id, index).unwrap().unwrap();
            sample.is_sync.then_some(sample)
        })
        .unwrap();
    (config, sample.bytes)
}

fn audio_configuration(rate: u32, bitrate: u32) -> mp4::TrackConfig {
    mp4::TrackConfig {
        track_type: mp4::TrackType::Audio,
        timescale: rate,
        language: "und".into(),
        media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
            bitrate,
            profile: mp4::AudioObjectType::AacLowComplexity,
            freq_index: match rate {
                16_000 => mp4::SampleFreqIndex::Freq16000,
                48_000 => mp4::SampleFreqIndex::Freq48000,
                _ => panic!("unsupported fixture sample rate"),
            },
            chan_conf: mp4::ChannelConfig::Mono,
        }),
    }
}

fn audio_sample(rate: u32, start_ms: u64, cts: i32, marker: u8) -> mp4::Mp4Sample {
    // ponytail: Payload markers qualify byte-preserving remux; decoded calibration is separate.
    mp4::Mp4Sample {
        start_time: start_ms * u64::from(rate) / 1_000,
        duration: rate / 50,
        rendering_offset: cts,
        is_sync: true,
        bytes: vec![marker, 0x55, 0xaa].into(),
    }
}

fn source_video_sample(sequence: u64, payload: &bytes::Bytes) -> mp4::Mp4Sample {
    mp4::Mp4Sample {
        start_time: sequence * 90_000,
        duration: 90_000,
        rendering_offset: 450,
        is_sync: true,
        bytes: payload.clone(),
    }
}

fn write_source(
    path: &Path,
    recording: u8,
    audio_rate: Option<u32>,
    first_audio_ms: u64,
    audio_cts: i32,
    audio_bitrate: u32,
    late_audio_ms: u64,
) -> Vec<CatalogMediaFragment> {
    let (video, payload) = video_configuration();
    let mut tracks = vec![video];
    if let Some(rate) = audio_rate {
        tracks.push(audio_configuration(rate, audio_bitrate));
    }
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap()],
        timescale: 1_000,
    };
    let mut writer =
        mp4::FragmentedMp4Writer::write_start(File::create(path).unwrap(), &config, &tracks)
            .unwrap();
    let initialization = writer.initialization();
    let mut fragments = Vec::new();
    let origin_ms = 10_000 + i64::from(recording) * 2_000;
    for sequence in 0..2 {
        writer
            .write_sample(1, source_video_sample(sequence, &payload))
            .unwrap();
        if let Some(rate) = audio_rate {
            let marker = recording * 3 + 1;
            if sequence == 0 {
                writer
                    .write_sample(2, audio_sample(rate, first_audio_ms, audio_cts, marker))
                    .unwrap();
                writer
                    .write_sample(2, audio_sample(rate, 200, 0, marker + 1))
                    .unwrap();
            } else {
                writer
                    .write_sample(2, audio_sample(rate, late_audio_ms, 0, marker + 2))
                    .unwrap();
            }
        }
        let fragment = writer.flush_fragment().unwrap().unwrap();
        fragments.push(CatalogMediaFragment {
            recording_id: format!("source-{recording}"),
            recording_started_at_ms: origin_ms,
            path: path.to_string_lossy().into_owned(),
            init_offset: initialization.offset,
            init_len: initialization.size,
            sequence: u64::from(fragment.sequence_number),
            start_ms: origin_ms + i64::try_from(sequence).unwrap() * 1_000,
            duration_ms: 1_000,
            byte_offset: fragment.range.offset,
            byte_len: fragment.range.size,
            archive: None,
        });
    }
    writer.write_end().unwrap();
    fragments
}

fn read_output(path: &Path) -> Output {
    let bytes = std::fs::read(path).unwrap();
    let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
    let tracks = reader
        .tracks()
        .iter()
        .map(|(&id, track)| {
            (
                id,
                track.track_type().unwrap(),
                track.timescale(),
                track.sample_count(),
            )
        })
        .collect::<Vec<_>>();
    let mut output = Output {
        bytes,
        video: Vec::new(),
        audio: Vec::new(),
        audio_timescale: None,
    };
    for (id, kind, timescale, count) in tracks {
        let samples = (1..=count)
            .map(|index| reader.read_sample(id, index).unwrap().unwrap())
            .collect();
        if kind == mp4::TrackType::Video {
            assert_eq!(timescale, 90_000);
            output.video = samples;
        } else {
            assert_eq!(kind, mp4::TrackType::Audio);
            output.audio_timescale = Some(timescale);
            output.audio = samples;
        }
    }
    output
}

fn audio_timeline(output: &Output) -> Vec<(u64, i128, u32, Vec<u8>)> {
    output
        .audio
        .iter()
        .map(|sample| {
            (
                sample.start_time,
                i128::from(sample.start_time) + i128::from(sample.rendering_offset),
                sample.duration,
                sample.bytes.to_vec(),
            )
        })
        .collect()
}

fn expected_audio(rate: u32, rows: &[(u64, u64, u8)]) -> Vec<(u64, i128, u32, Vec<u8>)> {
    rows.iter()
        .map(|&(start_ms, duration_ms, marker)| {
            (
                start_ms * u64::from(rate) / 1_000,
                i128::from(start_ms * u64::from(rate) / 1_000),
                u32::try_from(duration_ms * u64::from(rate) / 1_000).unwrap(),
                vec![marker, 0x55, 0xaa],
            )
        })
        .collect()
}

#[test]
fn audio_sync_positive_delay_preserves_video_and_cross_recording_audio_payloads() {
    for rate in [16_000, 48_000] {
        let source_cts = -i32::try_from(rate / 10).unwrap();
        let fixture = Fixture::new(Some(rate), 50, source_cts);
        let baseline = fixture.export("baseline.mp4", 0);
        assert_eq!(
            i128::from(baseline.audio[0].start_time)
                + i128::from(baseline.audio[0].rendering_offset),
            -i128::from(rate / 20)
        );
        let corrected = fixture.export("delayed.mp4", 200);
        assert_eq!(corrected.video, baseline.video);
        assert_eq!(corrected.audio_timescale, Some(rate));
        let mut expected = expected_audio(
            rate,
            &[
                (250, 150, 1),
                (400, 20, 2),
                (1_300, 20, 3),
                (2_250, 150, 4),
                (2_400, 20, 5),
                (3_300, 20, 6),
            ],
        );
        expected[0].1 = i128::from(rate) * 150 / 1_000;
        expected[3].1 = i128::from(rate) * 2_150 / 1_000;
        assert_eq!(audio_timeline(&corrected), expected);
        assert_eq!(corrected.audio[0].rendering_offset, source_cts);
    }
}

#[test]
fn audio_sync_negative_advance_drops_whole_negative_samples_without_clamping() {
    let fixture = Fixture::new(Some(48_000), 50, 0);
    let baseline = fixture.export("baseline.mp4", 0);
    let corrected = fixture.export("advanced.mp4", -100);
    assert_eq!(corrected.video, baseline.video);
    assert_eq!(
        audio_timeline(&corrected),
        expected_audio(
            48_000,
            &[
                (100, 20, 2),
                (1_000, 20, 3),
                (1_950, 150, 4),
                (2_100, 20, 5),
                (3_000, 20, 6),
            ]
        )
    );
    assert!(corrected.audio.iter().all(|sample| sample.bytes[0] != 1));
}

#[test]
fn audio_sync_negative_pts_is_dropped_even_when_mapped_dts_is_positive() {
    let fixture = Fixture::new(Some(16_000), 150, -1_280);
    let corrected = fixture.export("negative-pts.mp4", -100);
    assert_eq!(corrected.audio[0].bytes.as_ref(), &[2, 0x55, 0xaa]);
    assert_eq!(corrected.audio[0].start_time, 1_600);
    let later = corrected
        .audio
        .iter()
        .find(|sample| sample.bytes[0] == 4)
        .unwrap();
    assert_eq!(later.start_time, 32_800);
    assert_eq!(later.rendering_offset, -1_280);
    assert_eq!(
        i128::from(later.start_time) + i128::from(later.rendering_offset),
        31_520
    );
    assert!(corrected.audio.iter().all(|sample| sample.bytes[0] != 1));
    assert!(
        corrected
            .video
            .iter()
            .all(|sample| sample.rendering_offset == 450)
    );
}

#[test]
fn audio_sync_zero_is_byte_compatible_with_the_existing_export_entrypoint() {
    let fixture = Fixture::new(Some(48_000), 50, 0);
    let corrected = fixture.export("zero.mp4", 0);
    let path = fixture.root.join("default.mp4");
    export_fragment_ranges(&fixture.fragments, 14_000, &path, || false).unwrap();
    let existing = read_output(&path);
    assert_eq!(
        audio_timeline(&existing),
        expected_audio(
            48_000,
            &[
                (50, 150, 1),
                (200, 20, 2),
                (1_100, 20, 3),
                (2_050, 150, 4),
                (2_200, 20, 5),
                (3_100, 20, 6),
            ]
        )
    );
    assert_eq!(
        existing
            .video
            .iter()
            .map(|sample| (sample.start_time, sample.rendering_offset, sample.duration))
            .collect::<Vec<_>>(),
        vec![
            (0, 450, 90_000),
            (90_000, 450, 90_000),
            (180_000, 450, 90_000),
            (270_000, 450, 90_000)
        ]
    );
    assert_eq!(corrected.bytes, existing.bytes);
    assert_eq!(corrected.video, existing.video);
    assert_eq!(corrected.audio, existing.audio);
    fixture.assert_originals();
}

#[test]
fn low_bitrate_aac_description_is_registered_before_interleaved_video() {
    let fixture = Fixture::with_audio_bitrate(Some(48_000), 50, 0, 32_000);
    for offset in [0, 200] {
        let name = format!("low-bitrate-{offset}.mp4");
        let output = fixture.export(&name, offset);
        let shift = u64::try_from(offset).unwrap();
        assert_eq!(output.video.len(), 4);
        assert_eq!(
            audio_timeline(&output),
            expected_audio(
                48_000,
                &[
                    (50 + shift, 150, 1),
                    (200 + shift, 20, 2),
                    (1_100 + shift, 20, 3),
                    (2_050 + shift, 150, 4),
                    (2_200 + shift, 20, 5),
                    (3_100 + shift, 20, 6),
                ],
            )
        );
        let reader = mp4::read_mp4(File::open(fixture.root.join(name)).unwrap()).unwrap();
        let audio = reader
            .tracks()
            .values()
            .find(|track| track.track_type().unwrap() == mp4::TrackType::Audio)
            .unwrap();
        assert_eq!(audio.sample_description_count(), 1);
        let mp4::MediaConfig::AacConfig(config) = audio.media_config_for_description(1).unwrap()
        else {
            panic!("export must preserve AAC");
        };
        assert_eq!(config.bitrate, 32_000);
    }
    fixture.assert_originals();
}

#[test]
fn audio_sync_no_audio_is_unchanged_at_zero_and_nonzero_fails_before_publication() {
    let fixture = Fixture::new(None, 50, 0);
    let output = fixture.export("zero.mp4", 0);
    assert!(output.audio.is_empty());
    assert_eq!(output.audio_timescale, None);
    assert_eq!(output.video.len(), 4);
    let default_path = fixture.root.join("default.mp4");
    export_fragment_ranges(&fixture.fragments, 14_000, &default_path, || false).unwrap();
    assert_eq!(output.bytes, std::fs::read(default_path).unwrap());
    for offset in [-100, 200] {
        let destination = fixture.root.join(format!("unsupported-{offset}.mp4"));
        assert!(
            export_fragment_ranges_with_audio_offset(
                &fixture.fragments,
                14_000,
                &destination,
                offset,
                || false,
                |_| {},
            )
            .is_err()
        );
        fixture.assert_no_partial(&destination);
    }
}

#[test]
fn audio_sync_all_clipped_audio_fails_without_publishing_video_only_output() {
    let fixture = Fixture::new(Some(16_000), 50, 0);
    let destination = fixture.root.join("all-clipped.mp4");
    assert!(
        export_fragment_ranges_with_audio_offset(
            &fixture.fragments[..1],
            10_150,
            &destination,
            -100,
            || false,
            |_| {},
        )
        .is_err()
    );
    fixture.assert_no_partial(&destination);
}

#[test]
fn audio_sync_cancellation_removes_partial_output_and_preserves_originals() {
    let fixture = Fixture::new(Some(48_000), 50, 0);
    let destination = fixture.root.join("cancelled.mp4");
    let cancelled = Cell::new(false);
    assert!(
        export_fragment_ranges_with_audio_offset(
            &fixture.fragments,
            14_000,
            &destination,
            100,
            || cancelled.get(),
            |_| cancelled.set(true),
        )
        .is_err()
    );
    assert!(
        cancelled.get(),
        "fixture must reach actual output before cancellation"
    );
    fixture.assert_no_partial(&destination);
}
