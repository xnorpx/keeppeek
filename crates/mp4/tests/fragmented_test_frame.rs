use bytes::Bytes;
use mp4::{
    AvcConfig, FragmentedMp4Writer, MediaConfig, Mp4Config, Mp4Reader, Mp4Sample, TrackConfig,
    TrackType,
};
use std::{
    fs::File,
    io::BufReader,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};
use test_frame::{Codec, EncodedFrame, TestFrameConfig, TestFrameSource};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const FPS: u32 = 30;
const VIDEO_TIMESCALE: u32 = 90_000;
const FRAME_DURATION: u32 = VIDEO_TIMESCALE / FPS;
const TARGET_FRAMES: usize = 90;

#[test]
fn fragmented_test_frame_is_readable_and_seekable() {
    let frames = collect_frames();
    assert!(frames[0].is_keyframe, "fixture must begin with a keyframe");
    assert!(
        frames.iter().filter(|frame| frame.is_keyframe).count() >= 3,
        "fixture must contain at least three GOPs"
    );

    let first_avcc = annexb_to_avcc(&frames[0].data);
    let (sps, pps) = h264_parameters(&first_avcc);
    let output = output_path();
    let file = File::create(&output).unwrap();
    let config = Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec![
            "iso6".parse().unwrap(),
            "isom".parse().unwrap(),
            "mp41".parse().unwrap(),
        ],
        timescale: 1_000,
    };
    let track = TrackConfig {
        track_type: TrackType::Video,
        timescale: VIDEO_TIMESCALE,
        language: "und".to_owned(),
        media_conf: MediaConfig::AvcConfig(AvcConfig {
            width: WIDTH as u16,
            height: HEIGHT as u16,
            seq_param_set: sps,
            pic_param_set: pps,
        }),
    };
    let mut writer = FragmentedMp4Writer::write_start(file, &config, &[track]).unwrap();
    let initialization = writer.initialization();
    let mut fragment_ranges = Vec::new();
    let mut keyframe_locations = Vec::new();
    let mut expected_samples = Vec::new();

    for frame in &frames {
        if frame.is_keyframe
            && writer.has_pending_samples()
            && let Some(fragment) = writer.flush_fragment().unwrap()
        {
            fragment_ranges.push(fragment.range);
            keyframe_locations.push(fragment.video_keyframe.unwrap());
        }
        let avcc = annexb_to_avcc(&frame.data);
        expected_samples.push((frame.is_keyframe, avcc.clone()));
        writer
            .write_sample(
                1,
                Mp4Sample {
                    start_time: frame.frame_index * u64::from(FRAME_DURATION),
                    duration: FRAME_DURATION,
                    rendering_offset: 0,
                    is_sync: frame.is_keyframe,
                    bytes: Bytes::from(avcc),
                },
            )
            .unwrap();
    }
    if let Some(fragment) = writer.write_end().unwrap() {
        fragment_ranges.push(fragment.range);
        keyframe_locations.push(fragment.video_keyframe.unwrap());
    }
    drop(writer.into_writer());

    let size = std::fs::metadata(&output).unwrap().len();
    assert_eq!(initialization.offset, 0);
    assert_eq!(fragment_ranges[0].offset, initialization.size);
    for ranges in fragment_ranges.windows(2) {
        assert_eq!(ranges[1].offset, ranges[0].offset + ranges[0].size);
    }
    for (location, expected) in keyframe_locations.iter().zip(
        expected_samples
            .iter()
            .filter_map(|(is_sync, bytes)| is_sync.then_some(bytes)),
    ) {
        let start = usize::try_from(location.offset).unwrap();
        let end = start + location.size as usize;
        assert_eq!(&std::fs::read(&output).unwrap()[start..end], expected);
    }
    let last = fragment_ranges.last().unwrap();
    assert_eq!(last.offset + last.size, size);

    let mut reader =
        Mp4Reader::read_header(BufReader::new(File::open(&output).unwrap()), size).unwrap();
    assert!(reader.is_fragmented());
    assert_eq!(reader.moofs.len(), fragment_ranges.len());
    assert_eq!(reader.sample_count(1).unwrap(), frames.len() as u32);
    let decoder = reader.tracks()[&1].video_decoder_config().unwrap().unwrap();
    assert!(decoder.codec.starts_with("avc1."));
    assert_eq!(
        (decoder.width, decoder.height),
        (WIDTH as u16, HEIGHT as u16)
    );
    assert_eq!(decoder.nal_length_size, 4);
    assert!(!decoder.description.is_empty());
    let indexed_fragments = reader.fragment_first_sample_locations(1).unwrap();
    assert_eq!(indexed_fragments.len(), fragment_ranges.len());
    assert!(indexed_fragments.iter().all(|fragment| fragment.is_sync));
    for (index, fragment) in indexed_fragments.iter().enumerate() {
        assert_eq!(fragment.sequence_number, index as u32 + 1);
        assert_eq!(fragment.location, keyframe_locations[index]);
    }
    for (index, (expected_sync, expected_bytes)) in expected_samples.iter().enumerate() {
        let sample = reader.read_sample(1, index as u32 + 1).unwrap().unwrap();
        assert_eq!(sample.is_sync, *expected_sync);
        assert_eq!(sample.duration, FRAME_DURATION);
        assert_eq!(sample.bytes.as_ref(), expected_bytes);
    }

    let file_bytes = std::fs::read(&output).unwrap();
    let selected = fragment_ranges.last().unwrap();
    let mut standalone_fragment = file_bytes[..initialization.size as usize].to_vec();
    standalone_fragment.extend_from_slice(
        &file_bytes[selected.offset as usize..(selected.offset + selected.size) as usize],
    );
    let standalone_output = output.with_file_name("fragmented-test-frame-standalone-gop.mp4");
    std::fs::write(&standalone_output, &standalone_fragment).unwrap();
    let mut standalone_reader = Mp4Reader::read_header(
        std::io::Cursor::new(standalone_fragment.clone()),
        standalone_fragment.len() as u64,
    )
    .unwrap();
    let final_gop_start = frames.iter().rposition(|frame| frame.is_keyframe).unwrap();
    assert_eq!(
        standalone_reader.sample_count(1).unwrap(),
        (frames.len() - final_gop_start) as u32
    );
    assert!(
        standalone_reader
            .read_sample(1, 1)
            .unwrap()
            .unwrap()
            .is_sync
    );

    println!("fragmented MP4 fixture: {}", output.display());
    println!("standalone GOP fixture: {}", standalone_output.display());
}

fn collect_frames() -> Vec<EncodedFrame> {
    let (tx, rx) = mpsc::sync_channel(128);
    let source = TestFrameSource::start(
        TestFrameConfig {
            width: WIDTH,
            height: HEIGHT,
            fps: FPS,
            codec: Codec::H264,
            keyframe_interval: FPS,
            bitrate_bps: 500_000,
        },
        move |frame| {
            let _ = tx.send(frame);
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut frames = Vec::with_capacity(TARGET_FRAMES);
    while frames.len() < TARGET_FRAMES {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out collecting test frames");
        frames.push(rx.recv_timeout(remaining).unwrap());
    }
    source.stop();
    frames
}

fn output_path() -> PathBuf {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("test-output");
    std::fs::create_dir_all(&directory).unwrap();
    directory.join("fragmented-test-frame.mp4")
}

fn annexb_to_avcc(annexb: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(annexb.len());
    let mut position = 0;
    while let Some((start, prefix_len)) = find_start_code(annexb, position) {
        let nalu_start = start + prefix_len;
        let nalu_end = find_start_code(annexb, nalu_start).map_or(annexb.len(), |(next, _)| next);
        if nalu_start < nalu_end {
            let nalu = &annexb[nalu_start..nalu_end];
            output.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
            output.extend_from_slice(nalu);
        }
        position = nalu_end;
    }
    output
}

fn find_start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut position = from;
    while position + 3 <= data.len() {
        if data[position..].starts_with(&[0, 0, 1]) {
            return Some((position, 3));
        }
        if data[position..].starts_with(&[0, 0, 0, 1]) {
            return Some((position, 4));
        }
        position += 1;
    }
    None
}

fn h264_parameters(avcc: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut sps = None;
    let mut pps = None;
    let mut position = 0;
    while position + 4 <= avcc.len() {
        let size = u32::from_be_bytes(avcc[position..position + 4].try_into().unwrap()) as usize;
        position += 4;
        let end = position + size;
        if end > avcc.len() || size == 0 {
            break;
        }
        match avcc[position] & 0x1f {
            7 => sps = Some(avcc[position..end].to_vec()),
            8 => pps = Some(avcc[position..end].to_vec()),
            _ => {}
        }
        position = end;
    }
    (
        sps.expect("first keyframe must contain an SPS"),
        pps.expect("first keyframe must contain a PPS"),
    )
}
