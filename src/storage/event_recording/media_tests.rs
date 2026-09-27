use super::*;
use crate::storage::{
    catalog::{RecordingCatalog, RecordingCatalogHandle},
    frame::{AudioCodec, AudioFrame, MediaFrame, VideoCodec, VideoFrame},
    medium_term::MediumTermWriter,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
};

fn fixture(codec: VideoCodec) -> Vec<RecordingFrame> {
    let suffix = match codec {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "crates/test-camera/testdata/cc-4k-640x360-{suffix}.mp4"
    ));
    let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
    let (&id, track) = reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().ok() == Some(mp4::TrackType::Video))
        .unwrap();
    let decoder = track.video_decoder_config().unwrap().unwrap();
    let config = track.media_config_for_description(1).unwrap();
    let parameters: Vec<Vec<u8>> = match config {
        mp4::MediaConfig::AvcConfig(config) => vec![config.seq_param_set, config.pic_param_set],
        mp4::MediaConfig::HevcConfig(config) => vec![config.vps, config.sps, config.pps],
        _ => panic!("fixture must use H.264 or H.265"),
    };
    let timescale = track.timescale();
    let count = track.sample_count();
    let start = Instant::now() - Duration::from_secs(60);
    (0..count * 6)
        .map(|index| {
            let sample = reader.read_sample(id, index % count + 1).unwrap().unwrap();
            let mut bytes = Vec::new();
            if sample.is_sync {
                for parameter in &parameters {
                    bytes.extend_from_slice(&u32::try_from(parameter.len()).unwrap().to_be_bytes());
                    bytes.extend_from_slice(parameter);
                }
            }
            bytes.extend_from_slice(&sample.bytes);
            RecordingFrame {
                received_at: start
                    + Duration::from_secs(u64::from(index / count))
                    + Duration::from_secs_f64(sample.start_time as f64 / f64::from(timescale)),
                timestamp: None,
                frame: MediaFrame::Video(VideoFrame {
                    codec,
                    is_keyframe: sample.is_sync,
                    width: u32::from(decoder.width),
                    height: u32::from(decoder.height),
                    data: bytes.into(),
                }),
            }
        })
        .collect()
}

fn audio_fixture(start: Instant) -> Vec<RecordingFrame> {
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=16000",
            "-t",
            "6",
            "-c:a",
            "aac",
            "-f",
            "adts",
            "pipe:1",
        ])
        .output()
        .expect("ffmpeg is required for recording qualification");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut packets = Vec::new();
    let mut data = output.stdout.as_slice();
    while !data.is_empty() {
        assert!(data.len() >= 7);
        let length = (usize::from(data[3] & 3) << 11)
            | (usize::from(data[4]) << 3)
            | usize::from(data[5] >> 5);
        assert!((7..=data.len()).contains(&length));
        packets.push(RecordingFrame {
            received_at: start + Duration::from_millis(u64::try_from(packets.len()).unwrap() * 64),
            timestamp: None,
            frame: MediaFrame::Audio(AudioFrame {
                codec: AudioCodec::Aac,
                sample_rate: 16000,
                duration: Duration::from_millis(64),
                data: bytes::Bytes::copy_from_slice(&data[..length]),
            }),
        });
        data = &data[length..];
    }
    assert!(packets.len() > 80, "fixture must contain sustained audio");
    packets
}

fn ingest(runtime: &mut EventRecordings, stream: &str, frame: RecordingFrame) {
    let now = frame.received_at;
    let input = QueuedEventFrame::try_new(
        frame,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        64 * 1024 * 1024,
        1024,
    )
    .unwrap_or_else(|_| panic!("fixture fits queue"));
    runtime.ingest(
        RecordingStreamIdentity::new("camera", stream, "camera"),
        input,
        now,
    );
}

fn drain(
    runtime: &mut EventRecordings,
    writer: &mut Option<MediumTermWriter>,
    root: &std::path::Path,
    now: Instant,
    paths: &mut Vec<PathBuf>,
    catalog: &RecordingCatalogHandle,
) -> Vec<Instant> {
    let mut timestamps = Vec::new();
    while let Some(output) = runtime.next_output(now) {
        match output {
            EventOutput::Frame { identity, frame } => {
                let expected =
                    if runtime.cameras["camera"].settings.mode == CameraRecordingMode::EventBoost {
                        "sub"
                    } else {
                        "main"
                    };
                assert_eq!(
                    identity,
                    RecordingStreamIdentity::new("camera", expected, "camera")
                );
                if writer.is_none() {
                    *writer = Some(
                        MediumTermWriter::create_with_catalog_identity(
                            &root.join("media"),
                            identity,
                            frame.received_at,
                            8192,
                            catalog.clone(),
                        )
                        .unwrap(),
                    );
                }
                timestamps.push(frame.received_at);
                writer.as_mut().unwrap().append_received(frame).unwrap();
            }
            EventOutput::Finish { end, .. } => {
                if let Some(writer) = writer.take() {
                    paths.push(writer.finalize_before(end).unwrap());
                }
            }
        }
    }
    timestamps
}

fn verify_event_clip(codec: VideoCodec) {
    let mut frames = fixture(codec);
    let start = frames[0].received_at;
    frames.extend(audio_fixture(start));
    frames.sort_by_key(|frame| frame.received_at);
    let event = start + Duration::from_millis(2500);
    let deadline = event + Duration::from_secs(2);
    let root =
        std::env::temp_dir().join(format!("keeppeek-preroll-decode-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
    let handle = catalog.handle();
    let mut runtime = EventRecordings::new(64 * 1024 * 1024, 256 * 1024 * 1024);
    assert!(runtime.configure(
        "camera",
        EventSettings {
            mode: CameraRecordingMode::EventOnly,
            stream: EventRecordingStream::Main,
            pre: Duration::from_millis(1300),
            post: Duration::from_secs(2)
        }
    ));
    let mut writer = None;
    let mut paths = Vec::new();
    let mut triggered = false;
    let mut written = 0;
    for frame in frames {
        let now = frame.received_at;
        if now >= event && !triggered {
            assert!(
                !root.join("media").exists(),
                "idle history must not create files"
            );
            assert_eq!(handle.stats().unwrap().recording_files, 0);
            runtime.note_event("camera", event);
            triggered = true;
        }
        ingest(&mut runtime, "main", frame);
        written += drain(&mut runtime, &mut writer, &root, now, &mut paths, &handle).len();
    }
    written += drain(
        &mut runtime,
        &mut writer,
        &root,
        deadline + Duration::from_secs(1),
        &mut paths,
        &handle,
    )
    .len();
    assert_eq!(paths.len(), 1, "one event must produce one recording");
    assert_eq!(handle.stats().unwrap().recording_files, 1);
    assert_eq!(handle.stats().unwrap().finalized_files, 1);
    verify_saved_clip(&paths[0], written);
    verify_catalog_seek(&handle, "camera/main", &paths[0]);
    drop(handle);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

fn verify_saved_clip(path: &Path, written: usize) {
    let reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
    let samples: usize = reader
        .tracks()
        .values()
        .map(|track| track.sample_count() as usize)
        .sum();
    assert_eq!(samples, written);
    assert!(written > 1, "fixture must exercise dependent frames");
    let duration = verify_audio_coverage(path);
    assert!(
        duration > 2.0,
        "saved video must include history before the event"
    );
    assert!(
        duration <= 3.3,
        "saved video cannot exceed pre plus post duration"
    );
    verify_decode(path, false);
}

fn verify_audio_coverage(path: &Path) -> f64 {
    let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
    let tracks: Vec<_> = reader
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
        .collect();
    let mut video = None;
    let mut audio = Vec::new();
    for (id, kind, timescale, count) in tracks {
        assert!(count > 0);
        let mut end = 0;
        for index in 1..=count {
            let sample = reader.read_sample(id, index).unwrap().unwrap();
            assert!(
                sample.start_time >= end,
                "samples cannot overlap or regress"
            );
            end = sample.start_time + u64::from(sample.duration);
            if kind == mp4::TrackType::Audio {
                audio.push((
                    sample.start_time as f64 / f64::from(timescale),
                    end as f64 / f64::from(timescale),
                ));
            }
        }
        if kind == mp4::TrackType::Video {
            let first = reader.read_sample(id, 1).unwrap().unwrap();
            video = Some((
                first.start_time as f64 / f64::from(timescale),
                end as f64 / f64::from(timescale),
            ));
        }
    }
    let (video_start, video_end) = video.unwrap();
    assert!(audio.len() > 10, "clip must retain real AAC packets");
    for (start, end) in audio {
        assert!(start >= video_start, "audio starts before video");
        assert!(
            end <= video_end,
            "audio ends at {end}, beyond video {video_end}"
        );
    }
    video_end - video_start
}

fn verify_catalog_seek(handle: &RecordingCatalogHandle, stream: &str, path: &Path) {
    let now = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
    let fragments = handle
        .media_fragments_in_range(stream, now - 120_000, now)
        .unwrap();
    assert!(
        fragments.len() >= 2,
        "saved clip must expose catalog seek points"
    );
    let first = &fragments[0];
    assert!(
        fragments
            .iter()
            .all(|fragment| fragment.recording_id == first.recording_id)
    );
    let mut source = File::open(&first.path).unwrap();
    let mut clip = Vec::new();
    for (offset, length) in [
        (first.init_offset, first.init_len),
        (first.byte_offset, first.byte_len),
    ] {
        source.seek(SeekFrom::Start(offset)).unwrap();
        let mut bytes = vec![0; usize::try_from(length).unwrap()];
        source.read_exact(&mut bytes).unwrap();
        clip.extend(bytes);
    }
    let seek_path = path.with_extension("seek.mp4");
    std::fs::write(&seek_path, clip).unwrap();
    let mut reader = mp4::read_mp4(File::open(&seek_path).unwrap()).unwrap();
    let id = *reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().unwrap() == mp4::TrackType::Video)
        .unwrap()
        .0;
    assert!(reader.read_sample(id, 1).unwrap().unwrap().is_sync);
    verify_decode(&seek_path, false);
}

fn verify_decode(path: &Path, audio_only: bool) {
    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-xerror", "-i"])
        .arg(path)
        .args(if audio_only {
            vec!["-map", "0:a"]
        } else {
            vec![]
        })
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        decoded.status.success(),
        "independent clip decode failed: {}",
        String::from_utf8_lossy(&decoded.stderr)
    );
}

#[test]
fn event_preroll_h264_is_independently_decodable() {
    verify_event_clip(VideoCodec::H264);
}

#[test]
fn event_preroll_h265_is_independently_decodable() {
    verify_event_clip(VideoCodec::H265);
}

#[test]
fn overlapping_media_event_storm_writes_each_frame_once_to_one_catalog_recording() {
    let frames = fixture(VideoCodec::H264);
    let start = frames[0].received_at;
    let first = start + Duration::from_millis(2500);
    let deadline = start + Duration::from_millis(5500);
    let expected: Vec<_> = frames
        .iter()
        .map(|frame| frame.received_at)
        .filter(|at| *at >= start + Duration::from_secs(2) && *at < deadline)
        .collect();
    let root =
        std::env::temp_dir().join(format!("keeppeek-preroll-storm-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
    let handle = catalog.handle();
    let mut runtime = EventRecordings::new(64 * 1024 * 1024, 256 * 1024 * 1024);
    assert!(runtime.configure(
        "camera",
        EventSettings {
            mode: CameraRecordingMode::EventOnly,
            stream: EventRecordingStream::Main,
            pre: Duration::from_millis(1300),
            post: Duration::from_secs(2),
        }
    ));
    let mut writer = None;
    let mut paths = Vec::new();
    let mut actual = Vec::new();
    let mut events = (0..=1000)
        .map(|millis| first + Duration::from_millis(millis))
        .peekable();
    for frame in frames {
        let now = frame.received_at;
        while events.peek().is_some_and(|at| *at <= now) {
            runtime.note_event("camera", events.next().unwrap());
        }
        ingest(&mut runtime, "main", frame);
        actual.extend(drain(
            &mut runtime,
            &mut writer,
            &root,
            now,
            &mut paths,
            &handle,
        ));
    }
    actual.extend(drain(
        &mut runtime,
        &mut writer,
        &root,
        deadline + Duration::from_secs(1),
        &mut paths,
        &handle,
    ));
    assert_eq!(events.count(), 0);
    assert_eq!(
        actual, expected,
        "frame receive timestamps uniquely identify fixture inputs"
    );
    verify_storm_recording(&paths, &handle, expected.len());
    drop(handle);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

fn verify_storm_recording(paths: &[PathBuf], handle: &RecordingCatalogHandle, frames: usize) {
    assert_eq!(paths.len(), 1);
    assert_eq!(handle.stats().unwrap().recording_files, 1);
    assert_eq!(handle.stats().unwrap().finalized_files, 1);
    let reader = mp4::read_mp4(File::open(&paths[0]).unwrap()).unwrap();
    assert_eq!(
        reader
            .tracks()
            .values()
            .map(|track| track.sample_count() as usize)
            .sum::<usize>(),
        frames
    );
    verify_decode(&paths[0], false);
}

fn boost_fixture() -> Vec<(&'static str, RecordingFrame)> {
    let mut frames = Vec::new();
    let start = Instant::now() - Duration::from_secs(60);
    for (stream, codec, offset) in [("sub", VideoCodec::H264, 0), ("main", VideoCodec::H265, 10)] {
        let mut video = fixture(codec);
        let origin = video[0].received_at;
        for frame in &mut video {
            frame.received_at =
                start + Duration::from_millis(offset) + frame.received_at.duration_since(origin);
        }
        video.extend(audio_fixture(start + Duration::from_millis(offset)));
        frames.extend(video.into_iter().map(|frame| (stream, frame)));
    }
    frames.sort_by_key(|(_, frame)| frame.received_at);
    frames
}

#[test]
fn enabled_boost_preserves_one_recording_across_real_h264_h265_audio_transitions() {
    let frames = boost_fixture();
    let start = frames[0].1.received_at;
    let event = start + Duration::from_millis(2500);
    let root =
        std::env::temp_dir().join(format!("keeppeek-preroll-boost-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
    let handle = catalog.handle();
    let mut runtime = EventRecordings::new(64 * 1024 * 1024, 256 * 1024 * 1024);
    assert!(runtime.configure(
        "camera",
        EventSettings {
            mode: CameraRecordingMode::EventBoost,
            stream: EventRecordingStream::Main,
            pre: Duration::from_millis(1300),
            post: Duration::from_secs(2),
        }
    ));
    let mut writer = None;
    let mut paths = Vec::new();
    let mut triggered = false;
    for (stream, frame) in frames {
        let now = frame.received_at;
        if now >= event && !triggered {
            runtime.note_event("camera", event);
            triggered = true;
        }
        ingest(&mut runtime, stream, frame);
        drain(&mut runtime, &mut writer, &root, now, &mut paths, &handle);
    }
    let end = start + Duration::from_secs(7);
    runtime.begin_shutdown(end);
    drain(&mut runtime, &mut writer, &root, end, &mut paths, &handle);
    if let Some(writer) = writer {
        paths.push(writer.finalize().unwrap());
    }
    assert_eq!(
        paths.len(),
        1,
        "stream switches must preserve the recording identity"
    );
    assert_eq!(handle.stats().unwrap().recording_files, 1);
    assert_eq!(handle.stats().unwrap().finalized_files, 1);
    verify_audio_coverage(&paths[0]);
    verify_decode(&paths[0], true);
    verify_codec_runs(&paths[0]);
    verify_catalog_seek(&handle, "camera/sub", &paths[0]);
    drop(handle);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

fn verify_codec_runs(path: &Path) {
    let mut reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
    let (&id, track) = reader
        .tracks()
        .iter()
        .find(|(_, track)| track.track_type().unwrap() == mp4::TrackType::Video)
        .unwrap();
    assert_eq!(track.sample_description_count(), 2);
    let count = track.sample_count();
    let descriptions: Vec<_> = (1..=count)
        .map(|index| track.sample_description_index(index).unwrap())
        .collect();
    let mut runs: Vec<(u32, Vec<u8>)> = Vec::new();
    for (index, description) in descriptions.into_iter().enumerate() {
        let sample = reader
            .read_sample(id, u32::try_from(index + 1).unwrap())
            .unwrap()
            .unwrap();
        if runs
            .last()
            .is_none_or(|(previous, _)| *previous != description)
        {
            assert!(
                sample.is_sync,
                "every codec transition must start on a keyframe"
            );
            runs.push((description, Vec::new()));
        }
        let bytes = &mut runs.last_mut().unwrap().1;
        let mut data = sample.bytes.as_ref();
        while !data.is_empty() {
            let length =
                usize::try_from(u32::from_be_bytes(data[..4].try_into().unwrap())).unwrap();
            bytes.extend_from_slice(&[0, 0, 0, 1]);
            bytes.extend_from_slice(&data[4..4 + length]);
            data = &data[4 + length..];
        }
    }
    assert_eq!(
        runs.iter()
            .map(|(description, _)| *description)
            .collect::<Vec<_>>(),
        vec![1, 2, 1]
    );
    for (index, (description, bytes)) in runs.into_iter().enumerate() {
        // FFmpeg demuxers do not switch video codecs inside one MP4 track; decode each stored run.
        let extension = if description == 1 { "h264" } else { "hevc" };
        let raw = path.with_extension(format!("{index}.{extension}"));
        std::fs::write(&raw, bytes).unwrap();
        verify_decode(&raw, false);
    }
}
