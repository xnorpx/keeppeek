//! Streaming and snapshot command builders and types.
//!
//! Provides XML body construction for stream start/stop and snapshot requests.

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
///     <logicChannel>0</logicChannel>
///     <time>0</time>
///     <fullFrame>0</fullFrame>
///     <streamType>main</streamType>
///   </Snap>
/// </body>
/// ```
pub fn build_snapshot_request(req: &SnapshotRequest, buf: &mut [u8]) -> Result<usize, BcError> {
    xml::build_xml(buf, |b| {
        b.start_versioned("Snap", "1.1");
        b.u8_element("channelId", req.channel);
        b.u8_element("logicChannel", req.channel);
        b.u32_element("time", 0);
        b.u32_element("fullFrame", 0);
        b.text_element("streamType", "main");
        b.end();
    })
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
        assert!(xml_str.contains("<logicChannel>0</logicChannel>"));
        assert!(xml_str.contains("<fullFrame>0</fullFrame>"));
        assert!(xml_str.contains("<streamType>main</streamType>"));
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
}
