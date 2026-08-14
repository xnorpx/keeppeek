//! Streaming & two-way audio command builders and types.
//!
//! Provides XML body construction for stream start/stop, snapshot requests,
//! talk ability queries, talk configuration, and talk audio framing.

use crate::{error::BcError, header::PacketHeader, magic::*, xml};

/// Stream type selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamType {
    /// Main stream (high quality).
    Main,
    /// Sub stream (lower quality).
    Sub,
    /// External / third stream.
    Extern,
}

impl StreamType {
    /// Wire name used in XML `<streamType>` elements.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "mainStream",
            Self::Sub => "subStream",
            Self::Extern => "externStream",
        }
    }

    /// Decode stream type from Baichuan stream header wire id.
    pub const fn from_wire_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::Main),
            1 => Some(Self::Sub),
            2 => Some(Self::Extern),
            _ => None,
        }
    }
}

/// High-level stream subscription request (channel + stream type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSubscription {
    pub channel: u8,
    pub stream_type: StreamType,
    /// Expected resolution — used for auto-learn disambiguation when
    /// the camera doesn't differentiate streams in the BC header.
    pub expected_width: u32,
    pub expected_height: u32,
}

/// Parameters for a stream start request.
#[derive(Debug, Clone, Copy)]
pub struct StreamRequest {
    pub channel: u8,
    pub handle: u32,
    pub stream_type: StreamType,
}

/// Parameters for a stream stop request.
#[derive(Debug, Clone, Copy)]
pub struct StreamStop {
    pub channel: u8,
    pub handle: u32,
}

/// Parameters for a snapshot request.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotRequest {
    pub channel: u8,
}

/// Talk ability info parsed from camera response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TalkCapabilities {
    /// Number of audio channels (typically 1).
    pub audio_stream_mode: u32,
    /// 0 = half duplex, 1 = full duplex.
    pub duplex_mode: u32,
    /// Audio sample rate in Hz.
    pub sample_rate: u32,
    /// Audio bit depth (e.g. 8, 16).
    pub sample_precision: u32,
    /// Audio packet length in bytes.
    pub length_per_encoder: u32,
}

/// Build a stream start request XML body (msg_id 3).
///
/// ```xml
/// <body>
///   <Preview version="1.1">
///     <channelId>0</channelId>
///     <handle>0</handle>
///     <streamType>mainStream</streamType>
///   </Preview>
/// </body>
/// ```
pub fn build_stream_request(req: &StreamRequest, buf: &mut [u8]) -> Result<usize, BcError> {
    xml::build_xml(buf, |b| {
        b.start_versioned("Preview", "1.1");
        b.u8_element("channelId", req.channel);
        b.u32_element("handle", req.handle);
        b.text_element("streamType", req.stream_type.as_str());
        b.end();
    })
}

/// Build a stream stop request XML body (msg_id 4).
///
/// ```xml
/// <body>
///   <Preview version="1.1">
///     <channelId>0</channelId>
///     <handle>0</handle>
///   </Preview>
/// </body>
/// ```
pub fn build_stream_stop(stop: &StreamStop, buf: &mut [u8]) -> Result<usize, BcError> {
    xml::build_xml(buf, |b| {
        b.start_versioned("Preview", "1.1");
        b.u8_element("channelId", stop.channel);
        b.u32_element("handle", stop.handle);
        b.end();
    })
}

/// Build a snapshot request XML body (msg_id 109).
///
/// ```xml
/// <body>
///   <Snap version="1.1">
///     <channelId>0</channelId>
///   </Snap>
/// </body>
/// ```
pub fn build_snapshot_request(req: &SnapshotRequest, buf: &mut [u8]) -> Result<usize, BcError> {
    xml::build_xml(buf, |b| {
        b.start_versioned("Snap", "1.1");
        b.u8_element("channelId", req.channel);
        b.end();
    })
}

/// Build a talk capabilities query XML body (command 10).
///
/// ```xml
/// <body>
///   <TalkAbility version="1.1">
///     <channelId>0</channelId>
///   </TalkAbility>
/// </body>
/// ```
pub fn build_talk_capabilities_query(channel: u8, buf: &mut [u8]) -> Result<usize, BcError> {
    xml::build_xml(buf, |b| {
        b.start_versioned("TalkAbility", "1.1");
        b.u8_element("channelId", channel);
        b.end();
    })
}

/// Build a talk config XML body (msg_id 201).
///
/// ```xml
/// <body>
///   <TalkConfig version="1.1">
///     <channelId>0</channelId>
///     <duplex>0</duplex>
///     <audioStreamMode>0</audioStreamMode>
///     <audioConfig>
///       <sampleRate>8000</sampleRate>
///       <samplePrecision>16</samplePrecision>
///       <lengthPerEncoder>320</lengthPerEncoder>
///     </audioConfig>
///   </TalkConfig>
/// </body>
/// ```
pub fn build_talk_config(
    channel: u8,
    ability: &TalkCapabilities,
    buf: &mut [u8],
) -> Result<usize, BcError> {
    xml::build_xml(buf, |b| {
        b.start_versioned("TalkConfig", "1.1");
        b.u8_element("channelId", channel);
        b.u32_element("duplex", ability.duplex_mode);
        b.u32_element("audioStreamMode", ability.audio_stream_mode);
        b.start("audioConfig");
        b.u32_element("sampleRate", ability.sample_rate);
        b.u32_element("samplePrecision", ability.sample_precision);
        b.u32_element("lengthPerEncoder", ability.length_per_encoder);
        b.end(); // audioConfig
        b.end(); // TalkConfig
    })
}

/// Parse a talk capabilities response XML body.
pub fn parse_talk_capabilities(data: &[u8]) -> Result<TalkCapabilities, BcError> {
    let mut ability = TalkCapabilities {
        audio_stream_mode: 0,
        duplex_mode: 0,
        sample_rate: 8000,
        sample_precision: 16,
        length_per_encoder: 320,
    };

    xml::parse_xml(data, |name, text| {
        if let Ok(v) = text.parse::<u32>() {
            match name {
                "audioStreamMode" => ability.audio_stream_mode = v,
                "duplex" => ability.duplex_mode = v,
                "sampleRate" => ability.sample_rate = v,
                "samplePrecision" => ability.sample_precision = v,
                "lengthPerEncoder" => ability.length_per_encoder = v,
                _ => {}
            }
        }
    })?;

    Ok(ability)
}

/// Build the header for a stream start request (modern XML, extended).
///
/// Packs `channel`, `stream_type` and `msg_num` into `encryption_offset`
/// (bytes 12-15) following the Baichuan convention:
///   byte 12 = channel_id, byte 13 = stream_type, bytes 14-15 = msg_num.
pub const fn stream_request_header(
    body_len: usize,
    channel: u8,
    stream_type: StreamType,
    msg_num: u16,
) -> PacketHeader {
    let stream_type_id: u8 = match stream_type {
        StreamType::Main => 0,
        StreamType::Sub => 1,
        StreamType::Extern => 2,
    };
    let encryption_offset =
        (channel as u32) | ((stream_type_id as u32) << 8) | ((msg_num as u32) << 16);
    PacketHeader {
        msg_id: crate::COMMAND_STREAM,
        body_len: body_len as u32,
        encryption_offset,
        status_class: make_status(BC_CLASS_MODERN_EXT, 0),
        extension: Some(0),
    }
}

/// Build the header for a stream stop request (modern XML, extended).
pub const fn stream_stop_header(body_len: usize) -> PacketHeader {
    PacketHeader {
        msg_id: crate::COMMAND_PREVIEW_STOP,
        body_len: body_len as u32,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_MODERN_EXT, 0),
        extension: Some(0),
    }
}

/// Build the header for a snapshot request (modern XML, extended).
pub const fn snapshot_request_header(body_len: usize) -> PacketHeader {
    PacketHeader {
        msg_id: crate::COMMAND_SNAP,
        body_len: body_len as u32,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_MODERN_EXT, 0),
        extension: Some(0),
    }
}

/// Build the header for a talk capabilities query (modern XML, extended).
pub const fn talk_capabilities_query_header(body_len: usize) -> PacketHeader {
    PacketHeader {
        msg_id: crate::COMMAND_TALK_CAPABILITIES,
        body_len: body_len as u32,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_MODERN_EXT, 0),
        extension: Some(0),
    }
}

/// Build the header for a talk config request (modern XML, extended).
pub const fn talk_config_header(body_len: usize) -> PacketHeader {
    PacketHeader {
        msg_id: crate::COMMAND_TALK_CONFIG,
        body_len: body_len as u32,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_MODERN_EXT, 0),
        extension: Some(0),
    }
}

/// Build the header for a talk data packet (binary, legacy class).
pub const fn talk_data_header(body_len: usize) -> PacketHeader {
    PacketHeader {
        msg_id: crate::COMMAND_TALK,
        body_len: body_len as u32,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_LEGACY, 0),
        extension: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_type_str() {
        assert_eq!(StreamType::Main.as_str(), "mainStream");
        assert_eq!(StreamType::Sub.as_str(), "subStream");
        assert_eq!(StreamType::Extern.as_str(), "externStream");
    }

    #[test]
    fn build_stream_request_xml() {
        let req = StreamRequest {
            channel: 0,
            handle: 0,
            stream_type: StreamType::Main,
        };
        let mut buf = [0u8; 512];
        let len = build_stream_request(&req, &mut buf).unwrap();
        let xml_str = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml_str.contains("<Preview"));
        assert!(xml_str.contains("version=\"1.1\""));
        assert!(xml_str.contains("<channelId>0</channelId>"));
        assert!(xml_str.contains("<handle>0</handle>"));
        assert!(xml_str.contains("<streamType>mainStream</streamType>"));
    }

    #[test]
    fn build_stream_request_sub_channel1() {
        let req = StreamRequest {
            channel: 1,
            handle: 5,
            stream_type: StreamType::Sub,
        };
        let mut buf = [0u8; 512];
        let len = build_stream_request(&req, &mut buf).unwrap();
        let xml_str = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml_str.contains("<channelId>1</channelId>"));
        assert!(xml_str.contains("<handle>5</handle>"));
        assert!(xml_str.contains("<streamType>subStream</streamType>"));
    }

    #[test]
    fn build_stream_stop_xml() {
        let stop = StreamStop {
            channel: 0,
            handle: 0,
        };
        let mut buf = [0u8; 512];
        let len = build_stream_stop(&stop, &mut buf).unwrap();
        let xml_str = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml_str.contains("<Preview"));
        assert!(xml_str.contains("version=\"1.1\""));
        assert!(xml_str.contains("<channelId>0</channelId>"));
        assert!(xml_str.contains("<handle>0</handle>"));
        // Should NOT contain streamType
        assert!(!xml_str.contains("streamType"));
    }

    #[test]
    fn build_snapshot_request_xml() {
        let req = SnapshotRequest { channel: 0 };
        let mut buf = [0u8; 512];
        let len = build_snapshot_request(&req, &mut buf).unwrap();
        let xml_str = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml_str.contains("<Snap"));
        assert!(xml_str.contains("version=\"1.1\""));
        assert!(xml_str.contains("<channelId>0</channelId>"));
    }

    #[test]
    fn build_talk_ability_request_xml() {
        let mut buf = [0u8; 512];
        let len = build_talk_capabilities_query(0, &mut buf).unwrap();
        let xml_str = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml_str.contains("<TalkAbility"));
        assert!(xml_str.contains("version=\"1.1\""));
        assert!(xml_str.contains("<channelId>0</channelId>"));
    }

    #[test]
    fn build_talk_config_xml() {
        let ability = TalkCapabilities {
            audio_stream_mode: 0,
            duplex_mode: 1,
            sample_rate: 8000,
            sample_precision: 16,
            length_per_encoder: 320,
        };
        let mut buf = [0u8; 1024];
        let len = build_talk_config(0, &ability, &mut buf).unwrap();
        let xml_str = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml_str.contains("<TalkConfig"));
        assert!(xml_str.contains("version=\"1.1\""));
        assert!(xml_str.contains("<duplex>1</duplex>"));
        assert!(xml_str.contains("<sampleRate>8000</sampleRate>"));
        assert!(xml_str.contains("<samplePrecision>16</samplePrecision>"));
        assert!(xml_str.contains("<lengthPerEncoder>320</lengthPerEncoder>"));
    }

    #[test]
    fn parse_talk_ability_xml() {
        let xml = b"<body>\
            <TalkAbility version=\"1.1\">\
                <audioStreamMode>0</audioStreamMode>\
                <duplex>1</duplex>\
                <audioConfig>\
                    <sampleRate>16000</sampleRate>\
                    <samplePrecision>16</samplePrecision>\
                    <lengthPerEncoder>640</lengthPerEncoder>\
                </audioConfig>\
            </TalkAbility>\
        </body>";
        let ability = parse_talk_capabilities(xml).unwrap();
        assert_eq!(ability.audio_stream_mode, 0);
        assert_eq!(ability.duplex_mode, 1);
        assert_eq!(ability.sample_rate, 16000);
        assert_eq!(ability.sample_precision, 16);
        assert_eq!(ability.length_per_encoder, 640);
    }

    #[test]
    fn parse_talk_ability_defaults() {
        // Minimal XML with no recognized fields
        let xml = b"<body><TalkAbility version=\"1.1\"></TalkAbility></body>";
        let ability = parse_talk_capabilities(xml).unwrap();
        assert_eq!(ability.sample_rate, 8000);
        assert_eq!(ability.sample_precision, 16);
        assert_eq!(ability.length_per_encoder, 320);
    }

    #[test]
    fn stream_request_header_fields() {
        let hdr = stream_request_header(120, 0, StreamType::Main, 42);
        assert_eq!(hdr.msg_id, crate::COMMAND_STREAM);
        assert_eq!(hdr.body_len, 120);
        assert!(hdr.is_modern());
        assert!(hdr.is_extended());
        assert!(!hdr.is_binary());
        // channel=0, stream_type=0 (Main), msg_num=42
        assert_eq!(hdr.encryption_offset, 42 << 16);
    }

    #[test]
    fn stream_stop_header_fields() {
        let hdr = stream_stop_header(80);
        assert_eq!(hdr.msg_id, crate::COMMAND_PREVIEW_STOP);
        assert_eq!(hdr.body_len, 80);
    }

    #[test]
    fn snapshot_header_fields() {
        let hdr = snapshot_request_header(60);
        assert_eq!(hdr.msg_id, crate::COMMAND_SNAP);
        assert_eq!(hdr.body_len, 60);
    }

    #[test]
    fn talk_data_header_is_binary() {
        let hdr = talk_data_header(1024);
        assert_eq!(hdr.msg_id, crate::COMMAND_TALK);
        assert!(hdr.is_binary());
        assert!(!hdr.is_extended()); // LEGACY class has no extension
    }
}
