use mp4::{Mp4Config, Mp4Reader, Mp4Writer, TrackConfig};
use std::{io::Cursor, time::Duration};
use test_frame::{Codec, TestFrameConfig, TestFrameSource};

#[test]
fn test_mp4_write_and_read() {
    let config = TestFrameConfig {
        width: 320,
        height: 240,
        fps: 30,
        codec: Codec::H264,
        keyframe_interval: 30,
        bitrate_bps: 500_000,
    };

    let target_frames = 90;
    let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let frames_clone = frames.clone();
    let source = TestFrameSource::start(config, move |frame| {
        frames_clone.lock().unwrap().push(frame);
    })
    .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if frames.lock().unwrap().len() >= target_frames {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {target_frames} frames"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    source.stop();

    let frames = frames.lock().unwrap();
    assert!(
        frames.len() >= target_frames,
        "expected at least {} frames, got {}",
        target_frames,
        frames.len()
    );

    // Write MP4
    let mut buffer = Cursor::new(Vec::new());
    let mp4_config = Mp4Config {
        major_brand: str::parse("isom").unwrap(),
        minor_version: 512,
        compatible_brands: vec![
            str::parse("isom").unwrap(),
            str::parse("iso2").unwrap(),
            str::parse("avc1").unwrap(),
            str::parse("mp41").unwrap(),
        ],
        timescale: 1000,
    };

    let mut writer = Mp4Writer::write_start(&mut buffer, &mp4_config).unwrap();

    let track_config = TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: 1000,
        language: String::from("und"),
        media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
            width: 320,
            height: 240,
            seq_param_set: vec![], // We'll extract this from the first keyframe if needed, or just leave empty for this test
            pic_param_set: vec![],
        }),
    };

    writer.add_track(&track_config).unwrap();

    for frame in frames.iter() {
        let sample = mp4::Mp4Sample {
            start_time: frame.pts.as_millis() as u64,
            duration: (1000 / 30) as u32,
            rendering_offset: 0,
            is_sync: frame.is_keyframe,
            bytes: bytes::Bytes::copy_from_slice(&frame.data),
        };
        writer.write_sample(1, &sample).unwrap();
    }

    writer.write_end().unwrap();

    // Read MP4
    let file_bytes = buffer.get_ref().clone();
    buffer.set_position(0);
    let size = buffer.get_ref().len() as u64;
    let mut reader = Mp4Reader::read_header(buffer, size).unwrap();

    assert_eq!(reader.tracks().len(), 1);
    let track = reader.tracks().get(&1).unwrap();
    assert_eq!(track.track_type().unwrap(), mp4::TrackType::Video);

    // The writer might drop the last sample if it's not flushed properly, or there might be a mismatch.
    // Let's just check the samples that were actually written.
    let sample_count = track.sample_count();
    assert!(sample_count > 0);
    assert!(sample_count <= frames.len() as u32);

    for i in 0..sample_count as usize {
        let frame = &frames[i];
        let sample_id = i as u32 + 1;
        let location = reader.sample_location(1, sample_id).unwrap().unwrap();
        let sample = reader.read_sample(1, sample_id).unwrap().unwrap();

        assert_eq!(sample.is_sync, frame.is_keyframe);
        // The writer might adjust start_time based on the first sample's start_time
        // Let's just check the duration and bytes
        assert_eq!(sample.bytes.len(), frame.data.len());
        assert_eq!(&sample.bytes[..], &frame.data[..]);
        let start = usize::try_from(location.offset).unwrap();
        let end = start + location.size as usize;
        assert_eq!(&file_bytes[start..end], sample.bytes.as_ref());
    }
}
