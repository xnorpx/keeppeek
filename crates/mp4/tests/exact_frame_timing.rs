use std::{fs::File, path::Path, process::Command};

const DECODE_ORDER: [u8; 12] = [0, 3, 1, 2, 5, 4, 6, 9, 7, 8, 11, 10];
#[path = "../../../tests/fixtures/decoded_picture_ids.rs"]
mod decoded_picture_ids;

const VARIABLE_DURATIONS: [u32; 12] = [40, 40, 40, 120, 40, 40, 120, 40, 40, 120, 40, 40];

fn verify(name: &str, expected_pts: &[i128; 12], durations: [u32; 12]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/exact_frame_step")
        .join(name);
    let reader = mp4::read_mp4(File::open(&path).unwrap()).unwrap();
    let track = &reader.tracks()[&1];
    assert_eq!(track.timescale(), 1000);
    assert_eq!(track.sample_count(), 12);
    assert_eq!(track.sample_timing(0).unwrap(), None);
    assert_eq!(track.sample_timing(13).unwrap(), None);

    let mut presentation = Vec::new();
    let mut decode_time = 0;
    for (index, &frame_id) in DECODE_ORDER.iter().enumerate() {
        let ordinal = u32::try_from(index + 1).unwrap();
        let timing = track.sample_timing(ordinal).unwrap().unwrap();
        assert_eq!(
            timing.presentation_time,
            expected_pts[usize::from(frame_id)]
        );
        assert_eq!(
            (timing.decode_time, timing.duration),
            (decode_time, durations[index])
        );
        decode_time += u64::from(durations[index]);
        let position = reader.sample_location(1, ordinal).unwrap().unwrap().offset;
        presentation.push((timing.presentation_time, ordinal, frame_id, position));
    }
    presentation.sort_unstable();

    let ids = decoded_picture_ids::decoded_picture_ids(&path);
    let positions = decoded_positions(&path);
    assert_eq!(positions.len(), presentation.len());
    assert_eq!(ids.len(), presentation.len());
    for ((id, position), (_, _, expected_id, expected_position)) in
        ids.into_iter().zip(positions).zip(presentation)
    {
        assert_eq!((id, position), (expected_id, expected_position));
    }
}

fn decoded_positions(path: &Path) -> Vec<u64> {
    // Match each decoded picture to its encoded packet, even when two PTS values coincide.
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=pkt_pos",
            "-of",
            "compact=p=0:nk=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let probe_text = std::str::from_utf8(&probe.stdout).unwrap();
    probe_text
        .lines()
        .flat_map(|line| line.split('|'))
        .filter_map(|field| field.strip_prefix("pkt_pos="))
        .map(|number| number.parse().unwrap())
        .collect()
}

#[test]
fn sample_timing_matches_decoded_picture_id_for_cfr_vfr_and_duplicate_pts() {
    verify(
        "cfr.mp4",
        &[0, 40, 80, 120, 160, 200, 240, 280, 320, 360, 400, 440],
        [40; 12],
    );
    verify(
        "vfr.mp4",
        &[0, 40, 80, 200, 240, 280, 400, 440, 480, 600, 640, 680],
        VARIABLE_DURATIONS,
    );
    verify(
        "duplicate-pts.mp4",
        &[0, 40, 40, 200, 240, 280, 400, 440, 480, 600, 640, 680],
        VARIABLE_DURATIONS,
    );
}

#[test]
fn composition_offset_signedness_follows_the_fragment_version() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/exact_frame_step/vfr.mp4");
    let mut bytes = std::fs::read(path).unwrap();
    let run = bytes.windows(4).position(|tag| tag == b"trun").unwrap() + 4;
    // This fixture has duration, size and composition offset for each sample.
    assert_eq!(&bytes[run..run + 4], &[1, 0, 11, 5]);
    let offset = run + 16 + 8;
    bytes[offset..offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    for (version, expected) in [(0_u8, i128::from(u32::MAX)), (1, -1)] {
        bytes[run] = version;
        let mut reader =
            mp4::Mp4Reader::read_header(std::io::Cursor::new(&bytes), bytes.len() as u64).unwrap();
        assert_eq!(
            reader.tracks()[&1]
                .sample_timing(1)
                .unwrap()
                .unwrap()
                .presentation_time,
            expected
        );
        let sample = reader.read_sample(1, 1).unwrap().unwrap();
        assert_eq!(i128::from(sample.rendering_offset), expected);
        assert!(!sample.bytes.is_empty());
    }
}
