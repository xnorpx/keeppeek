//! Video & encoding query/config commands.
//!
//! Provides builders and parsers for video input, compression, stream catalogs,
//! OSD channel names, and privacy-mask settings.

use crate::{error::BcError, header::PacketHeader, magic::*, xml};
use arrayvec::{ArrayString, ArrayVec};

const NAME_CAP: usize = 64;

/// Video command (client → camera).
#[derive(Debug, Clone, Copy)]
pub enum VideoCommand {
    /// Read video input settings (ID 26).
    GetVideoInput { channel: u8 },
    /// Write video input settings (ID 25).
    SetVideoInput(VideoInputSettings),
    /// Read compression / encoding settings (ID 56).
    GetCompression { channel: u8 },
    /// Write compression / encoding settings (ID 57).
    SetCompression(CompressionSettings),
    /// Query available stream info list (ID 146).
    GetStreamCatalog,
    /// Read OSD channel name (ID 44).
    GetOsd { channel: u8 },
    /// Write OSD channel name (ID 45).
    SetOsd(OsdConfig),
    /// Read shelter / privacy mask (ID 52).
    GetShelter { channel: u8 },
    /// Write shelter / privacy mask (ID 53).
    SetShelter(ShelterConfig),
}

/// Video event (camera → client).
#[derive(Debug, Clone)]
pub enum VideoEvent {
    VideoInput(VideoInputSettings),
    VideoInputAck,
    Compression(CompressionProfiles),
    CompressionAck,
    StreamCatalog(StreamCatalog),
    Osd(OsdConfig),
    OsdAck,
    Shelter(ShelterConfig),
    ShelterAck,
}

/// Configured encoder settings returned for the camera's video streams.
#[derive(Debug, Clone, Copy)]
pub struct CompressionProfiles {
    pub main: Option<CompressionSettings>,
    pub sub: Option<CompressionSettings>,
}

/// Video input settings (brightness, contrast, etc).
#[derive(Debug, Clone, Copy)]
pub struct VideoInputSettings {
    pub channel: u8,
    pub brightness: u32,
    pub contrast: u32,
    pub saturation: u32,
    pub hue: u32,
    pub sharpness: u32,
}

/// Compression / encoding settings.
#[derive(Debug, Clone, Copy)]
pub struct CompressionSettings {
    pub channel: u8,
    pub stream_type: u8, // 0=main, 1=sub
    pub video_type: Option<ArrayString<16>>,
    pub resolution_width: u32,
    pub resolution_height: u32,
    pub bitrate: u32,
    pub fps: u32,
}

impl CompressionSettings {
    /// Set the video codec type (e.g. "h265", "h264").
    pub fn set_video_type(&mut self, vt: &str) {
        self.video_type = ArrayString::try_from(vt).ok();
    }

    /// Returns the video codec type as a string slice, or "unknown".
    pub fn video_type_str(&self) -> &str {
        self.video_type
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("unknown")
    }
}

/// Stream info list from response to ID 146.
#[derive(Debug, Clone)]
pub struct StreamCatalog {
    pub main_width: u32,
    pub main_height: u32,
    pub sub_width: u32,
    pub sub_height: u32,
    pub main_default_fps: u32,
    pub sub_default_fps: u32,
    pub main_framerates: ArrayVec<u32, 16>,
    pub sub_framerates: ArrayVec<u32, 16>,
}

/// OSD (on-screen display) channel name config.
#[derive(Debug, Clone, Copy)]
pub struct OsdConfig {
    pub channel: u8,
    pub enabled: bool,
    pub pos_x: u32,
    pub pos_y: u32,
    pub name: ArrayString<NAME_CAP>,
}

/// Shelter (privacy mask) config.
#[derive(Debug, Clone, Copy)]
pub struct ShelterConfig {
    pub channel: u8,
    pub enabled: bool,
    pub pos_x: u32,
    pub pos_y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum VideoResponseKind {
    VideoInput,
    VideoInputAck,
    Compression,
    CompressionAck,
    StreamCatalog,
    Osd,
    OsdAck,
    Shelter,
    ShelterAck,
}

/// Classify an incoming msg_id as a video response.
pub const fn classify_response(msg_id: u32) -> Option<VideoResponseKind> {
    match msg_id {
        crate::COMMAND_VIDEO_INPUT_READ => Some(VideoResponseKind::VideoInput),
        crate::COMMAND_VIDEO_INPUT_WRITE => Some(VideoResponseKind::VideoInputAck),
        crate::COMMAND_COMPRESSION_READ => Some(VideoResponseKind::Compression),
        crate::COMMAND_COMPRESSION_WRITE => Some(VideoResponseKind::CompressionAck),
        crate::COMMAND_STREAM_CATALOG => Some(VideoResponseKind::StreamCatalog),
        crate::COMMAND_OSD_READ => Some(VideoResponseKind::Osd),
        crate::COMMAND_OSD_WRITE => Some(VideoResponseKind::OsdAck),
        crate::COMMAND_SHELTER_READ => Some(VideoResponseKind::Shelter),
        crate::COMMAND_SHELTER_WRITE => Some(VideoResponseKind::ShelterAck),
        _ => None,
    }
}

pub fn build_request(cmd: &VideoCommand, buf: &mut [u8]) -> Result<(PacketHeader, usize), BcError> {
    match cmd {
        VideoCommand::GetVideoInput { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("VideoInput", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_VIDEO_INPUT_READ, len), len))
        }
        VideoCommand::SetVideoInput(settings) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("VideoInput", "1.1");
                b.u8_element("channelId", settings.channel);
                b.u32_element("bright", settings.brightness);
                b.u32_element("contrast", settings.contrast);
                b.u32_element("saturation", settings.saturation);
                b.u32_element("hue", settings.hue);
                b.u32_element("sharpen", settings.sharpness);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_VIDEO_INPUT_WRITE, len), len))
        }
        VideoCommand::GetCompression { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("Compression", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_COMPRESSION_READ, len), len))
        }
        VideoCommand::SetCompression(settings) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("Compression", "1.1");
                b.u8_element("channelId", settings.channel);
                b.u8_element("streamType", settings.stream_type);
                if let Some(ref vt) = settings.video_type {
                    b.text_element("videoType", vt.as_str());
                }
                b.u32_element("width", settings.resolution_width);
                b.u32_element("height", settings.resolution_height);
                b.u32_element("bitRate", settings.bitrate);
                b.u32_element("fps", settings.fps);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_COMPRESSION_WRITE, len), len))
        }
        VideoCommand::GetStreamCatalog => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("StreamInfoList", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_STREAM_CATALOG, len), len))
        }
        VideoCommand::GetOsd { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("OsdChannelName", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_OSD_READ, len), len))
        }
        VideoCommand::SetOsd(osd) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("OsdChannelName", "1.1");
                b.u8_element("channelId", osd.channel);
                b.text_element("enable", if osd.enabled { "1" } else { "0" });
                b.u32_element("posX", osd.pos_x);
                b.u32_element("posY", osd.pos_y);
                b.text_element("name", osd.name.as_str());
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_OSD_WRITE, len), len))
        }
        VideoCommand::GetShelter { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("Shelter", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_SHELTER_READ, len), len))
        }
        VideoCommand::SetShelter(shelter) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("Shelter", "1.1");
                b.u8_element("channelId", shelter.channel);
                b.text_element("enable", if shelter.enabled { "1" } else { "0" });
                b.u32_element("posX", shelter.pos_x);
                b.u32_element("posY", shelter.pos_y);
                b.u32_element("width", shelter.width);
                b.u32_element("height", shelter.height);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_SHELTER_WRITE, len), len))
        }
    }
}

pub fn parse_response(kind: VideoResponseKind, body: &[u8]) -> Result<VideoEvent, BcError> {
    match kind {
        VideoResponseKind::VideoInput => {
            let mut s = VideoInputSettings {
                channel: 0,
                brightness: 128,
                contrast: 128,
                saturation: 128,
                hue: 128,
                sharpness: 128,
            };
            xml::parse_xml(body, |name, text| {
                if let Ok(v) = text.parse::<u32>() {
                    match name {
                        "channelId" => s.channel = v as u8,
                        "bright" | "brightness" => s.brightness = v,
                        "contrast" => s.contrast = v,
                        "saturation" => s.saturation = v,
                        "hue" => s.hue = v,
                        "sharpen" | "sharpness" => s.sharpness = v,
                        _ => {}
                    }
                }
            })?;
            Ok(VideoEvent::VideoInput(s))
        }
        VideoResponseKind::VideoInputAck => Ok(VideoEvent::VideoInputAck),
        VideoResponseKind::Compression => {
            let mut channel = 0u8;
            let mut current_stream = None;
            let mut saw_nested_stream = false;
            let mut profiles = CompressionProfiles {
                main: None,
                sub: None,
            };
            let mut scalar = CompressionSettings {
                channel: 0,
                stream_type: 0,
                video_type: None,
                resolution_width: 0,
                resolution_height: 0,
                bitrate: 0,
                fps: 0,
            };
            xml::visit_xml(body, |event| match event {
                xml::XmlVisit::Start("mainStream") => {
                    current_stream = Some(0);
                    saw_nested_stream = true;
                }
                xml::XmlVisit::Start("subStream") => {
                    current_stream = Some(1);
                    saw_nested_stream = true;
                }
                xml::XmlVisit::Start("thirdStream") => {
                    current_stream = Some(2);
                    saw_nested_stream = true;
                }
                xml::XmlVisit::End("mainStream" | "subStream" | "thirdStream") => {
                    current_stream = None;
                }
                xml::XmlVisit::Text { name, text } => {
                    if name == "channelId" {
                        if let Ok(value) = text.parse::<u8>() {
                            channel = value;
                            scalar.channel = value;
                        }
                        return;
                    }
                    if name == "streamType" && current_stream.is_none() {
                        scalar.stream_type = match text {
                            "mainStream" => 0,
                            "subStream" => 1,
                            "thirdStream" | "externStream" => 2,
                            _ => text.parse::<u8>().unwrap_or(0),
                        };
                    }

                    let settings = match current_stream {
                        Some(0) => profiles.main.get_or_insert_with(|| compression_settings(0)),
                        Some(1) => profiles.sub.get_or_insert_with(|| compression_settings(1)),
                        Some(_) => return,
                        None => &mut scalar,
                    };
                    match name {
                        "videoType" | "vtype" => {
                            if let Ok(video_type) = ArrayString::try_from(text) {
                                settings.video_type = Some(video_type);
                            }
                        }
                        _ => {
                            if let Ok(value) = text.parse::<u32>() {
                                match name {
                                    "width" => settings.resolution_width = value,
                                    "height" => settings.resolution_height = value,
                                    "bitRate" | "bitrate" => settings.bitrate = value,
                                    "fps" | "frame" => settings.fps = value,
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                _ => {}
            })?;
            for settings in [&mut profiles.main, &mut profiles.sub]
                .into_iter()
                .flatten()
            {
                settings.channel = channel;
            }
            if !saw_nested_stream {
                match scalar.stream_type {
                    1 => profiles.sub = Some(scalar),
                    _ => profiles.main = Some(scalar),
                }
            }
            Ok(VideoEvent::Compression(profiles))
        }
        VideoResponseKind::CompressionAck => Ok(VideoEvent::CompressionAck),
        VideoResponseKind::StreamCatalog => {
            let mut info = StreamCatalog {
                main_width: 0,
                main_height: 0,
                sub_width: 0,
                sub_height: 0,
                main_default_fps: 0,
                sub_default_fps: 0,
                main_framerates: ArrayVec::new(),
                sub_framerates: ArrayVec::new(),
            };
            let mut current_stream = None;
            xml::visit_xml(body, |event| match event {
                xml::XmlVisit::Start("encodeTable") => current_stream = None,
                xml::XmlVisit::End("encodeTable") => current_stream = None,
                xml::XmlVisit::Text { name: "type", text } => {
                    current_stream = match text {
                        "mainStream" => Some(0),
                        "subStream" => Some(1),
                        _ => None,
                    };
                }
                xml::XmlVisit::Text { name, text } => {
                    if name == "framerateTable" {
                        let target = match current_stream {
                            Some(0) => &mut info.main_framerates,
                            Some(1) => &mut info.sub_framerates,
                            _ => return,
                        };
                        for value in text.split(',').filter_map(|value| value.parse().ok()) {
                            if !target.contains(&value) {
                                let _ = target.try_push(value);
                            }
                        }
                        return;
                    }
                    let Ok(value) = text.parse::<u32>() else {
                        return;
                    };
                    match (current_stream, name) {
                        (Some(0), "width") if info.main_width == 0 => info.main_width = value,
                        (Some(0), "height") if info.main_height == 0 => info.main_height = value,
                        (Some(0), "defaultFramerate") if info.main_default_fps == 0 => {
                            info.main_default_fps = value;
                        }
                        (Some(1), "width") if info.sub_width == 0 => info.sub_width = value,
                        (Some(1), "height") if info.sub_height == 0 => info.sub_height = value,
                        (Some(1), "defaultFramerate") if info.sub_default_fps == 0 => {
                            info.sub_default_fps = value;
                        }
                        (_, "mainWidth") => info.main_width = value,
                        (_, "mainHeight") => info.main_height = value,
                        (_, "subWidth") => info.sub_width = value,
                        (_, "subHeight") => info.sub_height = value,
                        _ => {}
                    }
                }
                _ => {}
            })?;
            Ok(VideoEvent::StreamCatalog(info))
        }
        VideoResponseKind::Osd => {
            let mut osd = OsdConfig {
                channel: 0,
                enabled: false,
                pos_x: 0,
                pos_y: 0,
                name: ArrayString::new(),
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        osd.channel = v;
                    }
                }
                "enable" => osd.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                "posX" => {
                    if let Ok(v) = text.parse::<u32>() {
                        osd.pos_x = v;
                    }
                }
                "posY" => {
                    if let Ok(v) = text.parse::<u32>() {
                        osd.pos_y = v;
                    }
                }
                "name" => {
                    let _ = ArrayString::try_from(text).map(|s| osd.name = s);
                }
                _ => {}
            })?;
            Ok(VideoEvent::Osd(osd))
        }
        VideoResponseKind::OsdAck => Ok(VideoEvent::OsdAck),
        VideoResponseKind::Shelter => {
            let mut s = ShelterConfig {
                channel: 0,
                enabled: false,
                pos_x: 0,
                pos_y: 0,
                width: 0,
                height: 0,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        s.channel = v;
                    }
                }
                "enable" => s.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                "posX" => {
                    if let Ok(v) = text.parse::<u32>() {
                        s.pos_x = v;
                    }
                }
                "posY" => {
                    if let Ok(v) = text.parse::<u32>() {
                        s.pos_y = v;
                    }
                }
                "width" => {
                    if let Ok(v) = text.parse::<u32>() {
                        s.width = v;
                    }
                }
                "height" => {
                    if let Ok(v) = text.parse::<u32>() {
                        s.height = v;
                    }
                }
                _ => {}
            })?;
            Ok(VideoEvent::Shelter(s))
        }
        VideoResponseKind::ShelterAck => Ok(VideoEvent::ShelterAck),
    }
}

const fn compression_settings(stream_type: u8) -> CompressionSettings {
    CompressionSettings {
        channel: 0,
        stream_type,
        video_type: None,
        resolution_width: 0,
        resolution_height: 0,
        bitrate: 0,
        fps: 0,
    }
}

const fn make_header(msg_id: u32, body_len: usize) -> PacketHeader {
    PacketHeader {
        msg_id,
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
    fn classify_video_ids() {
        assert!(matches!(
            classify_response(crate::COMMAND_OSD_READ),
            Some(VideoResponseKind::Osd)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_OSD_WRITE),
            Some(VideoResponseKind::OsdAck)
        ));
        assert!(classify_response(999).is_none());
    }

    #[test]
    fn build_get_video_input() {
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&VideoCommand::GetVideoInput { channel: 0 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_VIDEO_INPUT_READ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<VideoInput"));
        assert!(xml.contains("<channelId>0</channelId>"));
    }

    #[test]
    fn build_set_video_input() {
        let s = VideoInputSettings {
            channel: 0,
            brightness: 200,
            contrast: 100,
            saturation: 128,
            hue: 50,
            sharpness: 80,
        };
        let mut buf = [0u8; 1024];
        let (hdr, len) = build_request(&VideoCommand::SetVideoInput(s), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_VIDEO_INPUT_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<bright>200</bright>"));
        assert!(xml.contains("<contrast>100</contrast>"));
    }

    #[test]
    fn build_set_osd() {
        let osd = OsdConfig {
            channel: 0,
            enabled: true,
            pos_x: 10,
            pos_y: 20,
            name: ArrayString::try_from("Front Door").unwrap(),
        };
        let mut buf = [0u8; 1024];
        let (hdr, len) = build_request(&VideoCommand::SetOsd(osd), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_OSD_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<name>Front Door</name>"));
        assert!(xml.contains("<enable>1</enable>"));
    }

    #[test]
    fn parse_video_input_response() {
        let xml = b"<body>\
            <VideoInput version=\"1.1\">\
                <channelId>0</channelId>\
                <bright>180</bright>\
                <contrast>120</contrast>\
                <saturation>128</saturation>\
                <hue>128</hue>\
                <sharpen>64</sharpen>\
            </VideoInput>\
        </body>";
        let event = parse_response(VideoResponseKind::VideoInput, xml).unwrap();
        match event {
            VideoEvent::VideoInput(s) => {
                assert_eq!(s.brightness, 180);
                assert_eq!(s.contrast, 120);
                assert_eq!(s.sharpness, 64);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_osd_response() {
        let xml = b"<body>\
            <OsdChannelName version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
                <posX>100</posX>\
                <posY>200</posY>\
                <name>Garage</name>\
            </OsdChannelName>\
        </body>";
        let event = parse_response(VideoResponseKind::Osd, xml).unwrap();
        match event {
            VideoEvent::Osd(osd) => {
                assert!(osd.enabled);
                assert_eq!(osd.pos_x, 100);
                assert_eq!(osd.name.as_str(), "Garage");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_compression_response() {
        let xml = b"<body>\
            <Compression version=\"1.1\">\
                <channelId>0</channelId>\
                <streamType>0</streamType>\
                <width>1920</width>\
                <height>1080</height>\
                <bitRate>4096</bitRate>\
                <fps>25</fps>\
            </Compression>\
        </body>";
        let event = parse_response(VideoResponseKind::Compression, xml).unwrap();
        match event {
            VideoEvent::Compression(profiles) => {
                let c = profiles.main.unwrap();
                assert_eq!(c.resolution_width, 1920);
                assert_eq!(c.resolution_height, 1080);
                assert_eq!(c.bitrate, 4096);
                assert_eq!(c.fps, 25);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_nested_compression_profiles() {
        let xml = b"<body><Compression version=\"1.1\"><channelId>0</channelId>\
            <mainStream><width>2560</width><height>1440</height><frame>15</frame>\
            <bitRate>6144</bitRate></mainStream>\
            <subStream><width>640</width><height>360</height><frame>15</frame>\
            <bitRate>384</bitRate></subStream></Compression></body>";
        let event = parse_response(VideoResponseKind::Compression, xml).unwrap();
        let VideoEvent::Compression(profiles) = event else {
            panic!("wrong event");
        };
        let main = profiles.main.unwrap();
        let sub = profiles.sub.unwrap();
        assert_eq!(
            (main.resolution_width, main.resolution_height, main.fps),
            (2560, 1440, 15)
        );
        assert_eq!(
            (sub.resolution_width, sub.resolution_height, sub.fps),
            (640, 360, 15)
        );
    }

    #[test]
    fn parse_nested_stream_catalog() {
        let xml = b"<body><StreamInfoList version=\"1.1\"><StreamInfo>\
            <encodeTable><type>mainStream</type><resolution><width>3840</width>\
            <height>2160</height></resolution><defaultFramerate>25</defaultFramerate>\
            <framerateTable>25,20,15,10</framerateTable></encodeTable>\
            <encodeTable><type>subStream</type><resolution><width>640</width>\
            <height>360</height></resolution><defaultFramerate>10</defaultFramerate>\
            <framerateTable>15,10,7,4</framerateTable></encodeTable>\
            </StreamInfo></StreamInfoList></body>";
        let event = parse_response(VideoResponseKind::StreamCatalog, xml).unwrap();
        let VideoEvent::StreamCatalog(info) = event else {
            panic!("wrong event");
        };
        assert_eq!((info.main_width, info.main_height), (3840, 2160));
        assert_eq!((info.sub_width, info.sub_height), (640, 360));
        assert_eq!(info.main_default_fps, 25);
        assert_eq!(info.sub_default_fps, 10);
        assert_eq!(info.main_framerates.as_slice(), &[25, 20, 15, 10]);
        assert_eq!(info.sub_framerates.as_slice(), &[15, 10, 7, 4]);
    }

    #[test]
    fn unknown_fields_tolerated() {
        let xml = b"<body>\
            <VideoInput version=\"1.1\">\
                <bright>100</bright>\
                <futureField>unknown</futureField>\
            </VideoInput>\
        </body>";
        let event = parse_response(VideoResponseKind::VideoInput, xml).unwrap();
        match event {
            VideoEvent::VideoInput(s) => assert_eq!(s.brightness, 100),
            _ => panic!("wrong event"),
        }
    }
}
