use bytes::Bytes;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const MAX_AUDIO_QUEUE_BYTES: usize = 256 * 1024;
const MAX_AUDIO_QUEUE_AGE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    G711Alaw,
    G711Ulaw,
    PcmS16Le,
}

#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub(crate) codec: AudioCodec,
    pub(crate) sample_rate_hz: u32,
    pub(crate) channel_count: u8,
    pub(crate) timestamp: Option<Duration>,
    pub(crate) received_at: Instant,
    pub(crate) data: Bytes,
}

#[derive(Debug, Default)]
pub struct AudioQueue {
    frames: VecDeque<AudioFrame>,
    bytes: usize,
    dropped_frames: u64,
}

impl AudioQueue {
    pub fn push(&mut self, frame: AudioFrame, now: Instant) {
        self.expire(now);
        self.bytes = self.bytes.saturating_add(frame.data.len());
        self.frames.push_back(frame);
        while self.bytes > MAX_AUDIO_QUEUE_BYTES {
            self.drop_oldest();
        }
    }

    pub fn pop(&mut self, now: Instant) -> Option<AudioFrame> {
        self.expire(now);
        let frame = self.frames.pop_front()?;
        self.bytes = self.bytes.saturating_sub(frame.data.len());
        Some(frame)
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Reported by the WebRTC audio health path")
    )]
    pub const fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn expire(&mut self, now: Instant) {
        while self.frames.front().is_some_and(|frame| {
            now.saturating_duration_since(frame.received_at) > MAX_AUDIO_QUEUE_AGE
        }) {
            self.drop_oldest();
        }
    }

    fn drop_oldest(&mut self) {
        if let Some(frame) = self.frames.pop_front() {
            self.bytes = self.bytes.saturating_sub(frame.data.len());
            self.dropped_frames = self.dropped_frames.saturating_add(1);
        }
    }
}

pub fn decode_g711(codec: AudioCodec, data: &[u8]) -> Option<Bytes> {
    let decode = match codec {
        AudioCodec::G711Alaw => decode_alaw,
        AudioCodec::G711Ulaw => decode_ulaw,
        _ => return None,
    };
    let mut pcm = Vec::with_capacity(data.len().saturating_mul(2));
    for &sample in data {
        pcm.extend_from_slice(&decode(sample).to_le_bytes());
    }
    Some(Bytes::from(pcm))
}

pub fn encode_g711(codec: AudioCodec, pcm: &[u8]) -> Option<Bytes> {
    let encode = match codec {
        AudioCodec::G711Alaw => encode_alaw,
        AudioCodec::G711Ulaw => encode_ulaw,
        _ => return None,
    };
    let mut encoded = Vec::with_capacity(pcm.len() / 2);
    for &[low, high] in pcm.as_chunks::<2>().0 {
        encoded.push(encode(i16::from_le_bytes([low, high])));
    }
    Some(Bytes::from(encoded))
}

fn decode_alaw(encoded: u8) -> i16 {
    let value = encoded ^ 0x55;
    let magnitude = i16::from(value & 0x0f) << 4;
    let exponent = (value >> 4) & 0x07;
    let magnitude = if exponent == 0 {
        magnitude + 8
    } else {
        (magnitude + 0x108) << (exponent - 1)
    };
    if value & 0x80 == 0 {
        -magnitude
    } else {
        magnitude
    }
}

fn decode_ulaw(encoded: u8) -> i16 {
    let value = !encoded;
    let magnitude = ((i16::from(value & 0x0f) << 3) + 0x84) << ((value >> 4) & 0x07);
    if value & 0x80 == 0 {
        0x84 - magnitude
    } else {
        magnitude - 0x84
    }
}

fn encode_alaw(sample: i16) -> u8 {
    let sign: u32 = if sample < 0 { 0 } else { 0x80 };
    let magnitude = i32::from(sample).unsigned_abs().min(32_635);
    let exponent: u32 = if magnitude < 256 {
        0
    } else {
        31 - magnitude.leading_zeros() - 7
    };
    let mantissa: u32 = if exponent == 0 {
        magnitude >> 4
    } else {
        magnitude >> (exponent + 3)
    } & 0x0f;
    (sign | (exponent << 4) | mantissa) as u8 ^ 0x55
}

fn encode_ulaw(sample: i16) -> u8 {
    let sample = i32::from(sample);
    let sign = if sample < 0 { 0x80 } else { 0 };
    let magnitude = sample.unsigned_abs().min(32_635) as i32 + 0x84;
    let exponent = (31 - magnitude.leading_zeros()).saturating_sub(7);
    let mantissa = (magnitude >> (exponent + 3)) & 0x0f;
    !(sign | ((exponent as i32) << 4) | mantissa) as u8
}

#[cfg(test)]
mod tests {
    use super::{AudioCodec, AudioFrame, AudioQueue, decode_g711, encode_g711};
    use bytes::Bytes;
    use std::time::{Duration, Instant};

    fn frame(received_at: Instant, len: usize) -> AudioFrame {
        AudioFrame {
            codec: AudioCodec::G711Alaw,
            sample_rate_hz: 8_000,
            channel_count: 1,
            timestamp: None,
            received_at,
            data: Bytes::from(vec![0; len]),
        }
    }

    #[test]
    fn queue_drops_oldest_audio_when_byte_bound_is_reached() {
        let start = Instant::now();
        let mut queue = AudioQueue::default();
        queue.push(frame(start, 200 * 1024), start);
        queue.push(frame(start, 100 * 1024), start);

        assert_eq!(queue.dropped_frames(), 1);
        assert_eq!(queue.pop(start).unwrap().data.len(), 100 * 1024);
    }

    #[test]
    fn queue_expires_audio_before_it_can_accumulate_latency() {
        let start = Instant::now();
        let mut queue = AudioQueue::default();
        queue.push(frame(start, 1), start);

        assert!(queue.pop(start + Duration::from_millis(251)).is_none());
        assert_eq!(queue.dropped_frames(), 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn decodes_g711_to_little_endian_pcm() {
        let alaw = decode_g711(AudioCodec::G711Alaw, &[0xd5, 0x55]).unwrap();
        let ulaw = decode_g711(AudioCodec::G711Ulaw, &[0xff, 0x7f]).unwrap();

        assert_eq!(alaw.len(), 4);
        assert_eq!(ulaw.len(), 4);
        assert_eq!(i16::from_le_bytes([alaw[0], alaw[1]]), 8);
        assert_eq!(i16::from_le_bytes([ulaw[0], ulaw[1]]), 0);
        assert!(decode_g711(AudioCodec::Aac, &[0]).is_none());
    }

    #[test]
    fn encodes_pcm_for_both_g711_variants() {
        let pcm = [0_u8, 0_u8, 0xff, 0x7f, 0x00, 0x80];
        let alaw = encode_g711(AudioCodec::G711Alaw, &pcm).unwrap();
        let ulaw = encode_g711(AudioCodec::G711Ulaw, &pcm).unwrap();

        assert_eq!(alaw.len(), 3);
        assert_eq!(ulaw.len(), 3);
        assert_eq!(
            decode_g711(AudioCodec::G711Alaw, &alaw).unwrap().len(),
            pcm.len()
        );
        assert_eq!(
            decode_g711(AudioCodec::G711Ulaw, &ulaw).unwrap().len(),
            pcm.len()
        );
        assert!(encode_g711(AudioCodec::Aac, &pcm).is_none());
    }
}
