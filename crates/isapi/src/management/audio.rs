use std::collections::BTreeSet;
use std::fmt;

use super::{Query, boolean, nonzero, number, required};
use crate::error::Kind;
use crate::{Document, Error, Format, Method, Request};

/// A device-reported audio codec, without implying that an encoder is available.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AudioCodec {
    /// G.711 A-law, 8 kHz mono, one byte per sample.
    G711Alaw,
    /// G.711 mu-law, 8 kHz mono, one byte per sample.
    G711Ulaw,
    /// An unimplemented codec retained for capability reporting.
    Other(String),
}

impl AudioCodec {
    fn parse(value: String) -> Result<Self, Error> {
        if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            return Err(Error::new(Kind::Protocol));
        }
        Ok(match value.to_ascii_lowercase().as_str() {
            "g.711alaw" => Self::G711Alaw,
            "g.711ulaw" => Self::G711Ulaw,
            _ => Self::Other(value),
        })
    }

    /// Returns the canonical device codec name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::G711Alaw => "G.711alaw",
            Self::G711Ulaw => "G.711ulaw",
            Self::Other(value) => value,
        }
    }

    /// Returns the supported sample rate; unknown codecs have no inferred rate.
    pub const fn sample_rate_hz(&self) -> Option<u32> {
        match self {
            Self::G711Alaw | Self::G711Ulaw => Some(8000),
            Self::Other(_) => None,
        }
    }
}

impl fmt::Display for AudioCodec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A validated two-way audio channel with separate speaker and microphone evidence.
#[derive(Clone, Debug)]
pub struct AudioChannel {
    id: u32,
    enabled: bool,
    input_codec: AudioCodec,
    output_codec: AudioCodec,
    speaker: bool,
    microphone: bool,
    speaker_volume: Option<u32>,
    video_inputs: Vec<u32>,
    document: Document,
}

impl AudioChannel {
    /// Queries the camera's two-way audio channel list without starting audio.
    ///
    /// # Errors
    /// Rejects malformed channel lists, duplicate IDs and lists over 64 channels.
    pub fn list() -> Result<Query<Vec<Self>>, Error> {
        Query::new("/ISAPI/System/TwoWayAudio/channels", |document| {
            let nodes = document
                .root("TwoWayAudioChannelList")?
                .children("TwoWayAudioChannel");
            if nodes.len() > 64 {
                return Err(Error::new(Kind::Limit));
            }
            let mut ids = BTreeSet::new();
            nodes
                .into_iter()
                .map(|node| {
                    let channel = Self::parse(node.document())?;
                    if !ids.insert(channel.id) {
                        return Err(Error::new(Kind::Protocol));
                    }
                    Ok(channel)
                })
                .collect()
        })
    }

    /// Queries one channel without changing its configuration.
    ///
    /// # Errors
    /// Rejects channel zero or invalid response fields.
    pub fn query(channel: u32) -> Result<Query<Self>, Error> {
        Query::new(resource(channel, "")?, Self::parse)
    }

    fn parse(document: Document) -> Result<Self, Error> {
        let root = document.root("TwoWayAudioChannel")?;
        let id = number(root, "id")?
            .filter(|id| *id > 0)
            .ok_or_else(|| Error::new(Kind::Protocol))?;
        let output_codec = AudioCodec::parse(required(root, "audioCompressionType")?)?;
        let input_codec = root
            .field("audioInboundCompressionType")?
            .map(AudioCodec::parse)
            .transpose()?
            .unwrap_or_else(|| output_codec.clone());
        let forbidden = |name| -> Result<bool, Error> {
            root.child(name)?
                .map(|_| boolean(root, name))
                .transpose()
                .map(|value| value.unwrap_or(false))
        };
        let speaker_volume = number(root, "speakerVolume")?;
        if speaker_volume.is_some_and(|value| value > 100) {
            return Err(Error::new(Kind::Protocol));
        }
        let mut video_inputs = Vec::new();
        if let Some(associate) = root.child("associateVideoInputs")?
            && boolean(associate, "enabled")?
        {
            let list = associate
                .child("videoInputChannelList")?
                .ok_or_else(|| Error::new(Kind::Protocol))?;
            for value in list.children("videoInputChannelID") {
                let value = value
                    .text()?
                    .parse::<u32>()
                    .map_err(|_| Error::new(Kind::Protocol))?;
                if value == 0 || video_inputs.contains(&value) || video_inputs.len() >= 64 {
                    return Err(Error::new(Kind::Protocol));
                }
                video_inputs.push(value);
            }
        }
        Ok(Self {
            id,
            enabled: boolean(root, "enabled")?,
            speaker: !forbidden("lineOutForbidden")?,
            microphone: !forbidden("micInForbidden")?,
            input_codec,
            output_codec,
            speaker_volume,
            video_inputs,
            document,
        })
    }

    /// Returns the two-way audio channel ID, independent of RTSP stream numbering.
    pub const fn id(&self) -> u32 {
        self.id
    }
    /// Returns the reported configuration state, not session ownership.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }
    /// Returns whether audio output to the camera speaker is available.
    pub const fn speaker_supported(&self) -> bool {
        self.speaker
    }
    /// Returns whether the camera microphone can supply audio.
    pub const fn microphone_supported(&self) -> bool {
        self.microphone
    }
    /// Returns the camera microphone input codec reported by this channel.
    pub const fn input_codec(&self) -> &AudioCodec {
        &self.input_codec
    }
    /// Returns the camera speaker output codec required for uploaded audio.
    pub const fn output_codec(&self) -> &AudioCodec {
        &self.output_codec
    }
    /// Returns the reported speaker volume on the camera's 0..100 scale.
    pub const fn speaker_volume(&self) -> Option<u32> {
        self.speaker_volume
    }
    /// Returns explicit associated video input channels; an empty list is not an inferred mapping.
    pub fn video_inputs(&self) -> &[u32] {
        &self.video_inputs
    }
    /// Returns the original validated channel configuration.
    pub const fn document(&self) -> &Document {
        &self.document
    }

    /// Creates an explicit session-open operation with a typed session response.
    ///
    /// # Errors
    /// Returns request validation errors. The caller owns permission and exclusive channel use.
    pub fn open(&self) -> Result<Query<AudioSession>, Error> {
        Ok(Query::for_request(
            Request::put(resource(self.id, "/open")?, [])?,
            AudioSession::parse,
        ))
    }

    /// Creates an explicit session-close operation; it never sends it automatically.
    ///
    /// # Errors
    /// Returns request validation errors.
    pub fn close(&self) -> Result<Request, Error> {
        Request::put(resource(self.id, "/close")?, [])
    }
}

/// An opaque session identity returned by the camera's two-way audio open operation.
#[derive(Clone)]
pub struct AudioSession {
    id: String,
}

impl AudioSession {
    fn parse(document: Document) -> Result<Self, Error> {
        let id = required(document.root("TwoWayAudioSession")?, "sessionId")?;
        if id.len() > 256 || id.chars().any(char::is_control) {
            return Err(Error::new(Kind::Protocol));
        }
        Ok(Self { id })
    }

    /// Builds the binary upload handshake bound to this exact session.
    ///
    /// # Errors
    /// Rejects channel zero and oversized resource paths.
    pub fn send_request(&self, channel: u32) -> Result<Request, Error> {
        Request::with_body(
            Method::Put,
            self.audio_resource(channel)?,
            Format::Binary,
            [],
        )
    }

    /// Builds the microphone receive request bound to this exact session.
    ///
    /// # Errors
    /// Rejects channel zero and oversized resource paths.
    pub fn receive_request(&self, channel: u32) -> Result<Request, Error> {
        Request::get(self.audio_resource(channel)?)
    }

    fn audio_resource(&self, channel: u32) -> Result<String, Error> {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("sessionId", &self.id)
            .finish();
        Ok(format!("{}?{query}", resource(channel, "/audioData")?))
    }
}

impl fmt::Debug for AudioSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AudioSession")
            .finish_non_exhaustive()
    }
}

fn resource(channel: u32, suffix: &str) -> Result<String, Error> {
    Ok(format!(
        "/ISAPI/System/TwoWayAudio/channels/{}{suffix}",
        nonzero(channel)?
    ))
}
