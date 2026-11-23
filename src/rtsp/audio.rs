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
}
