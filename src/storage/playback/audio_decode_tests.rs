use super::*;
use sha2::{Digest, Sha256};

mod decode;
mod source;

// One AAC packet is 21.334 ms. The independent PCM detector uses 10 ms windows.
const DECODE_TOLERANCE_US: i64 = 32_000;

struct Fixture {
    root: PathBuf,
    fragments: Vec<CatalogMediaFragment>,
    digest: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("audio-decode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("source.mp4");
        let fragments = source::write(&path);
        assert_eq!(fragments.len(), 4);
        let digest = Sha256::digest(std::fs::read(path).unwrap()).into();
        Self {
            root,
            fragments,
            digest,
        }
    }

    fn assert_source(&self) {
        let observed: [u8; 32] =
            Sha256::digest(std::fs::read(self.root.join("source.mp4")).unwrap()).into();
        assert_eq!(observed, self.digest);
    }

    fn export(&self, offset: i32) -> PathBuf {
        let path = self.root.join(format!("export-{offset}.mp4"));
        let end = self.fragments.last().unwrap();
        export_fragment_ranges_with_audio_offset(
            &self.fragments,
            end.start_ms + i64::try_from(end.duration_ms).unwrap(),
            &path,
            offset,
            || false,
            |_| {},
        )
        .unwrap();
        self.assert_source();
        path
    }

    fn stored(&self, offset: i32, first: usize) -> PathBuf {
        self.stored_at(offset, first, 0)
    }

    fn stored_at(&self, offset: i32, first: usize, origin_ms: i64) -> PathBuf {
        let path = self
            .root
            .join(format!("stored-{offset}-{first}-{origin_ms}.mp4"));
        source::stored(&self.fragments, offset, first, origin_ms, &path);
        self.assert_source();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "failed audio decoder evidence retained at {}",
                self.root.display()
            );
            return;
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn assert_lag(observed: &decode::Observed, expected_us: i64) {
    assert_eq!(
        observed.flashes_us.len(),
        3,
        "fixture must retain all flashes"
    );
    assert_eq!(
        observed.pulses_us.len(),
        3,
        "fixture must retain all pulses"
    );
    for (&flash, &pulse) in observed.flashes_us.iter().zip(&observed.pulses_us) {
        let actual_us = pulse - flash;
        assert!(
            (actual_us - expected_us).abs() <= DECODE_TOLERANCE_US,
            "decoded A/V lag {actual_us}us differs from {expected_us}us"
        );
    }
}

#[test]
fn decoded_audio_correction_moves_pulses_and_preserves_video_in_exports_and_playback() {
    let fixture = Fixture::new();
    let baseline = decode::media(&fixture.root.join("source.mp4"));
    assert_lag(&baseline, -200_000);
    for offset in [-200, 0, 200] {
        for path in [fixture.export(offset), fixture.stored(offset, 0)] {
            let observed = decode::media(&path);
            assert_eq!(observed.flashes_us, baseline.flashes_us);
            assert_lag(&observed, -200_000 + i64::from(offset) * 1_000);
            eprintln!(
                "decoded-audio offset_ms={offset} file={} flashes_us={:?} pulses_us={:?}",
                path.file_name().unwrap().to_string_lossy(),
                observed.flashes_us,
                observed.pulses_us
            );
        }
    }
    fixture.assert_source();
}

#[test]
fn decoded_corrected_seek_uses_previous_fragment_audio_on_the_original_video_clock() {
    let fixture = Fixture::new();
    let baseline = decode::media(&fixture.root.join("source.mp4"));
    let origin_ms = fixture.fragments[1].start_ms;
    assert!(origin_ms >= 1_000);
    for origin in [0, origin_ms] {
        let path = fixture.stored_at(200, 1, origin);
        let observed = decode::media(&path);
        let expected = baseline
            .flashes_us
            .iter()
            .map(|time| time - origin * 1_000)
            .collect::<Vec<_>>();
        assert_eq!(observed.flashes_us, expected);
        assert_lag(&observed, 0);
        if origin != 0 {
            assert!(observed.flashes_us[0] < 1_000);
        }
    }
    fixture.assert_source();
}

#[test]
#[ignore = "run explicitly to measure recorded audio export and playback"]
fn measure_recorded_audio_file_operations() {
    let fixture = Fixture::new();
    let end = fixture.fragments.last().unwrap();
    let end_ms = end.start_ms + i64::try_from(end.duration_ms).unwrap();
    let path = fixture.root.join("measurement.mp4");
    let measure = |run, operation, offset, action: &dyn Fn()| {
        let started = Instant::now();
        action();
        let elapsed_us = started.elapsed().as_micros();
        let bytes = std::fs::metadata(&path).unwrap().len();
        std::fs::remove_file(&path).unwrap();
        if run > 0 {
            println!("audio-measurement,{run},{operation},{offset},{elapsed_us},{bytes}");
        }
    };
    for run in 0..=30 {
        let offsets = if run % 2 == 0 { [0, 200] } else { [200, 0] };
        for offset in offsets {
            measure(run, "export", offset, &|| {
                export_fragment_ranges_with_audio_offset(
                    &fixture.fragments,
                    end_ms,
                    &path,
                    offset,
                    || false,
                    |_| {},
                )
                .unwrap();
            });
            measure(run, "playback", offset, &|| {
                source::stored(&fixture.fragments, offset, 0, 0, &path);
            });
            measure(run, "seek", offset, &|| {
                source::stored(
                    &fixture.fragments,
                    offset,
                    1,
                    fixture.fragments[1].start_ms,
                    &path,
                );
            });
        }
    }
    fixture.assert_source();
}
