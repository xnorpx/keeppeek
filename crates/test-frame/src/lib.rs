use openh264::{
    OpenH264API,
    encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod},
    formats::YUVSlices,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod pattern;

#[derive(Debug, Clone, Copy)]
pub enum Codec {
    H264,
}

pub struct TestFrameConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec: Codec,
    pub keyframe_interval: u32,
    pub bitrate_bps: u32,
}

pub struct EncodedFrame {
    /// H.264 Annex B encoded data (start-code delimited NALUs).
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub frame_index: u64,
    pub pts: Duration,
}

pub struct TestFrameSource {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl TestFrameSource {
    pub fn start(
        config: TestFrameConfig,
        callback: impl FnMut(EncodedFrame) + Send + 'static,
    ) -> Result<Self, openh264::Error> {
        assert!(
            config.width.is_multiple_of(2) && config.height.is_multiple_of(2),
            "dimensions must be even"
        );
        assert!(config.fps > 0, "fps must be positive");
        assert!(
            config.keyframe_interval > 0,
            "keyframe_interval must be positive"
        );

        let width = config.width as usize;
        let height = config.height as usize;
        let fps = config.fps;
        let keyframe_interval = config.keyframe_interval;
        let frame_interval_ns = 1_000_000_000u64 / fps as u64;

        let enc_config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(config.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .skip_frames(true)
            .intra_frame_period(IntraFramePeriod::from_num_frames(keyframe_interval));

        let api = OpenH264API::from_source();
        let encoder = Encoder::with_api_config(api, enc_config)?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let handle = thread::Builder::new()
            .name("test-frame".into())
            .spawn(move || {
                run_source(
                    encoder,
                    callback,
                    &stop_clone,
                    width,
                    height,
                    fps,
                    frame_interval_ns,
                );
            })
            .expect("failed to spawn test-frame thread");

        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self);
    }
}

impl Drop for TestFrameSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run_source(
    mut encoder: Encoder,
    mut callback: impl FnMut(EncodedFrame),
    stop: &AtomicBool,
    width: usize,
    height: usize,
    fps: u32,
    frame_interval_ns: u64,
) {
    let y_size = width * height;
    let uv_size = (width / 2) * (height / 2);
    let mut flat = vec![0u8; y_size + uv_size * 2];
    let mut queue: VecDeque<EncodedFrame> = VecDeque::new();
    let mut frame_index = 0u64;

    let prebuffer = (fps * 2) as u64;
    for _ in 0..prebuffer {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if let Some(frame) = encode_one(
            &mut encoder,
            &mut flat,
            width,
            height,
            fps,
            frame_index,
            frame_interval_ns,
        ) {
            queue.push_back(frame);
        }
        frame_index += 1;
    }

    let start = Instant::now();
    let mut delivered = 0u64;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let target = start + Duration::from_nanos(frame_interval_ns * delivered);
        sleep_until(target, stop);
        if stop.load(Ordering::Relaxed) {
            break;
        }

        if let Some(frame) = queue.pop_front() {
            callback(frame);
        }
        delivered += 1;

        if let Some(frame) = encode_one(
            &mut encoder,
            &mut flat,
            width,
            height,
            fps,
            frame_index,
            frame_interval_ns,
        ) {
            queue.push_back(frame);
        }
        frame_index += 1;
    }
}

fn encode_one(
    encoder: &mut Encoder,
    flat: &mut [u8],
    width: usize,
    height: usize,
    fps: u32,
    frame_index: u64,
    frame_interval_ns: u64,
) -> Option<EncodedFrame> {
    let y_size = width * height;
    let uv_size = (width / 2) * (height / 2);

    {
        let (y, rest) = flat.split_at_mut(y_size);
        let (u, v) = rest.split_at_mut(uv_size);
        pattern::render(width, height, frame_index, fps, y, u, v);
    }

    let (y, rest) = flat.split_at(y_size);
    let (u, v) = rest.split_at(uv_size);
    let yuv = YUVSlices::new((y, u, v), (width, height), (width, width / 2, width / 2));
    let bs = encoder.encode(&yuv).ok()?;

    let data = bs.to_vec();
    if data.is_empty() {
        return None;
    }

    Some(EncodedFrame {
        is_keyframe: is_idr(&data),
        data,
        width: width as u32,
        height: height as u32,
        frame_index,
        pts: Duration::from_nanos(frame_interval_ns * frame_index),
    })
}

const fn is_idr(annexb: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 < annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 0 && annexb[i + 3] == 1 {
            let nal_type = annexb[i + 4] & 0x1F;
            if nal_type == 5 {
                return true;
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    false
}

fn sleep_until(target: Instant, stop: &AtomicBool) {
    loop {
        let now = Instant::now();
        if now >= target || stop.load(Ordering::Relaxed) {
            return;
        }
        let remaining = target - now;
        if remaining > Duration::from_millis(2) {
            thread::sleep(Duration::from_millis(1));
        } else {
            while Instant::now() < target {
                std::hint::spin_loop();
            }
            return;
        }
    }
}
