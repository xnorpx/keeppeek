use super::*;
use crate::storage::segment::{RecordingRtpEpoch, RecordingRtpTime};
use bytes::Bytes;
use std::sync::Arc;

const GOP_COUNT: u32 = 30;
const VIDEO_HZ: u32 = 90_000;
const AUDIO_HZ: u32 = 48_000;

fn clock(
    enabled: bool,
    epoch: &Arc<RecordingRtpEpoch>,
    rate: u32,
    ticks: u32,
) -> Option<RecordingRtpTime> {
    enabled.then(|| RecordingRtpTime {
        epoch: epoch.clone(),
        clock_rate: rate.try_into().unwrap(),
        timestamp: i64::from(ticks) + 3_000,
    })
}

fn workload(start: Instant, measured: bool) -> Vec<Vec<RecordingFrame>> {
    let epoch = |track| {
        Arc::new(RecordingRtpEpoch {
            session_id: [1; 16],
            epoch_id: [track; 16],
        })
    };
    let video_epoch = epoch(1);
    let audio_epoch = epoch(2);
    let mut inter = vec![0_u8; 2_052];
    inter[..5].copy_from_slice(&[0, 0, 8, 0, 0x41]);
    let inter = Bytes::from(inter);
    let audio = Bytes::from(vec![5_u8; 128]);
    let mut batches = Vec::new();
    let mut next_audio = 0;
    for gop in 0..GOP_COUNT {
        let mut frames = Vec::new();
        for ordinal in gop * 50..(gop + 1) * 50 {
            let ticks = ordinal * 3_600;
            let offset = Duration::from_millis(u64::from(ordinal) * 40);
            let mut frame = super::tests::video_frame(start + offset, offset);
            if !ordinal.is_multiple_of(50)
                && let MediaFrame::Video(video) = &mut frame.frame
            {
                video.is_keyframe = false;
                video.data = inter.clone();
            }
            frame.source_rtp = clock(measured, &video_epoch, VIDEO_HZ, ticks);
            frames.push(frame);
            while u64::from(next_audio) * u64::from(VIDEO_HZ)
                < u64::from(ticks + 3_600) * u64::from(AUDIO_HZ)
            {
                let timestamp = ticks_to_duration(u64::from(next_audio), AUDIO_HZ);
                frames.push(RecordingFrame {
                    received_at: start + timestamp,
                    timestamp: Some(timestamp),
                    source_rtp: clock(measured, &audio_epoch, AUDIO_HZ, next_audio),
                    frame: MediaFrame::Audio(crate::storage::AudioFrame {
                        codec: AudioCodec::Aac,
                        sample_rate: AUDIO_HZ,
                        duration: ticks_to_duration(1_024, AUDIO_HZ),
                        data: audio.clone(),
                    }),
                });
                next_audio += 1_024;
            }
        }
        batches.push(frames);
    }
    batches
}

fn recording(root: &Path, measured: bool) -> std::io::Result<(u128, u128, u64)> {
    let start = Instant::now();
    let batches = workload(start, measured);
    let started = Instant::now();
    let mut writer = MediumTermWriter::create(root, "clock", start, 64 * 1024)?;
    let mut admission = Vec::new();
    for frames in batches {
        let gop_started = Instant::now();
        writer.append_batch(frames)?;
        admission.push(gop_started.elapsed().as_nanos());
    }
    let path = writer.finalize()?;
    let elapsed = started.elapsed().as_nanos();
    let bytes = std::fs::metadata(&path)?.len();
    std::fs::remove_file(path)?;
    admission.sort_unstable();
    Ok((
        elapsed,
        admission[(admission.len() * 95).div_ceil(100) - 1],
        bytes,
    ))
}

#[test]
#[ignore = "paired local storage measurement; run alone with --ignored --nocapture"]
fn measure_original_rtp_recording() -> std::io::Result<()> {
    let root = std::env::temp_dir().join(format!("keeppeek-clock-{}", Uuid::new_v4()));
    for pair in 0..=30 {
        for measured in [pair % 2 == 0, pair % 2 != 0] {
            let (elapsed_ns, gop_p95_ns, bytes) = recording(&root, measured)?;
            if pair > 0 {
                println!("recording-clock,{pair},{measured},{elapsed_ns},{gop_p95_ns},{bytes}");
            }
        }
    }
    std::fs::remove_dir_all(root)
}
