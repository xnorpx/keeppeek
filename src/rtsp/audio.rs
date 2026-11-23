use crate::{storage::AudioCodec, webrtc::audio::AudioCodec as WebRtcAudioCodec};
use retina::{client::Stream, codec::ParametersRef};

pub(super) struct ObservedAudio<'a> {
    pub storage_codec: AudioCodec,
    pub live_codec: WebRtcAudioCodec,
    pub sample_rate: u32,
    pub channels: u8,
    pub decoder_config: &'a [u8],
}

impl<'a> ObservedAudio<'a> {
    pub(super) fn from_track(track: &'a Stream) -> Option<Self> {
        let ParametersRef::Audio(parameters) = track.parameters()? else {
            return None;
        };
        let (storage_codec, live_codec) = match track.encoding_name() {
            "mpeg4-generic" => (AudioCodec::Aac, WebRtcAudioCodec::Aac),
            "pcma" => (AudioCodec::G711Alaw, WebRtcAudioCodec::G711Alaw),
            "pcmu" => (AudioCodec::G711Ulaw, WebRtcAudioCodec::G711Ulaw),
            _ => return None,
        };
        Some(Self {
            storage_codec,
            live_codec,
            sample_rate: parameters.clock_rate(),
            channels: u8::try_from(parameters.channels().get()).ok()?,
            decoder_config: parameters.extra_data(),
        })
    }

    /// The MP4 writer emits a two-byte AudioSpecificConfig and a 16.16 sample rate.
    pub(super) fn recording_aac_config(&self) -> Option<mp4::AacConfig> {
        let &[first, second] = self.decoder_config else {
            return None;
        };
        if self.storage_codec != AudioCodec::Aac
            || second & 7 != 0
            || self.sample_rate > u32::from(u16::MAX)
        {
            return None;
        }
        // The three remaining GASpecificConfig flags must be zero. Extensions, explicit
        // frequencies and program-config layouts cannot be restated by this writer.
        let object_type = first >> 3;
        if !(1..=4).contains(&object_type) {
            return None;
        }
        let freq_index = mp4::SampleFreqIndex::try_from(((first & 7) << 1) | (second >> 7)).ok()?;
        let chan_conf = mp4::ChannelConfig::try_from((second >> 3) & 15).ok()?;
        if freq_index.freq() != self.sample_rate || chan_conf.channel_count() != self.channels {
            return None;
        }
        Some(mp4::AacConfig {
            profile: mp4::AudioObjectType::try_from(object_type).ok()?,
            freq_index,
            chan_conf,
            bitrate: 64_000,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_requires_an_exactly_representable_raw_aac_description() {
        let observed = |decoder_config: &'static [u8], sample_rate, channels| ObservedAudio {
            storage_codec: AudioCodec::Aac,
            live_codec: WebRtcAudioCodec::Aac,
            sample_rate,
            channels,
            decoder_config,
        };
        let main = observed(&[0x09, 0x90], 48_000, 2)
            .recording_aac_config()
            .unwrap();
        assert_eq!(main.profile, mp4::AudioObjectType::AacMain);
        assert_eq!(main.chan_conf, mp4::ChannelConfig::Stereo);
        let surround = observed(&[0x11, 0xb8], 48_000, 8)
            .recording_aac_config()
            .unwrap();
        assert_eq!(surround.chan_conf, mp4::ChannelConfig::SevenOne);
        for config in [
            observed(&[0x11, 0x94], 48_000, 2), // 960-sample frame flag
            observed(&[0x11, 0x90, 0x56, 0xe5, 0], 48_000, 2), // sync extension
            observed(&[0x11, 0x80], 48_000, 2), // program config layout
            observed(&[0x11, 0x90], 48_000, 1), // same object type with a different layout
            observed(&[0x10, 0x10], 96_000, 2), // the sample entry can't carry this rate
            observed(&[0x11, 0x90], 44_100, 2),
            observed(&[0xb9, 0x90], 48_000, 2), // low-delay profile needs additional flags
        ] {
            assert!(config.recording_aac_config().is_none());
            assert!(!config.decoder_config.is_empty());
        }
    }
}
