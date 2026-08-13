//! PTZ (Pan-Tilt-Zoom) control commands.
//!
//! Provides builders and parsers for PTZ move/stop, preset management,
//! zoom/focus control, and PTZ guard configuration.

use crate::{error::BcError, header::PacketHeader, magic::*, xml};
use arrayvec::ArrayString;

const NAME_CAP: usize = 64;

/// PTZ command (client → camera).
#[derive(Debug, Clone)]
pub enum PtzCommand {
    /// Move in a direction at given speed (ID 18). Speed is clamped to 1..=64.
    Move {
        channel: u8,
        direction: PtzDirection,
        speed: u8,
    },
    /// Stop PTZ movement (ID 18).
    Stop { channel: u8 },
    /// Query preset list (ID 190).
    PresetList { channel: u8 },
    /// Go to a preset position (ID 19).
    PresetGoto { channel: u8, preset_id: u32 },
    /// Save current position as a preset (ID 19).
    PresetSave {
        channel: u8,
        preset_id: u32,
        name: ArrayString<NAME_CAP>,
    },
    /// Delete a preset (ID 19).
    PresetDelete { channel: u8, preset_id: u32 },
    /// Read current zoom/focus values (ID 294).
    GetZoomFocus { channel: u8 },
    /// Start a zoom/focus operation (ID 295).
    StartZoomFocus { channel: u8, operation: ZoomFocusOp },
    /// Read PTZ guard config (ID 433).
    GetGuard { channel: u8 },
    /// Write PTZ guard config (ID 433).
    SetGuard(PtzGuardConfig),
}

/// PTZ movement direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtzDirection {
    Left,
    Right,
    Up,
    Down,
    LeftUp,
    LeftDown,
    RightUp,
    RightDown,
    ZoomIn,
    ZoomOut,
    FocusNear,
    FocusFar,
    Stop,
}

impl PtzDirection {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
            Self::LeftUp => "leftUp",
            Self::LeftDown => "leftDown",
            Self::RightUp => "rightUp",
            Self::RightDown => "rightDown",
            Self::ZoomIn => "zoomIn",
            Self::ZoomOut => "zoomOut",
            Self::FocusNear => "focusNear",
            Self::FocusFar => "focusFar",
            Self::Stop => "stop",
        }
    }
}

/// Zoom/focus operation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomFocusOp {
    ZoomIn,
    ZoomOut,
    FocusNear,
    FocusFar,
    Stop,
}

impl ZoomFocusOp {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ZoomIn => "zoomIn",
            Self::ZoomOut => "zoomOut",
            Self::FocusNear => "focusNear",
            Self::FocusFar => "focusFar",
            Self::Stop => "stop",
        }
    }
}

/// PTZ event (camera → client).
#[derive(Debug, Clone)]
pub enum PtzEvent {
    MoveAck,
    PresetList(Vec<PresetPosition>),
    PresetAck,
    ZoomFocus(ZoomFocusInfo),
    ZoomFocusAck,
    Guard(PtzGuardConfig),
    GuardAck,
}

/// A single PTZ preset entry.
#[derive(Debug, Clone)]
pub struct PresetPosition {
    pub id: u32,
    pub name: ArrayString<NAME_CAP>,
}

/// Zoom and focus position values.
#[derive(Debug, Clone, Copy)]
pub struct ZoomFocusInfo {
    pub zoom: u32,
    pub focus: u32,
}

/// PTZ guard configuration.
#[derive(Debug, Clone, Copy)]
pub struct PtzGuardConfig {
    pub channel: u8,
    pub enabled: bool,
    pub preset_id: u32,
    pub wait_time: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum PtzResponseKind {
    Move,
    PresetList,
    Preset,
    ZoomFocus,
    StartZoomFocus,
    Guard,
}

/// Classify an incoming msg_id as a PTZ response.
pub const fn classify_response(msg_id: u32) -> Option<PtzResponseKind> {
    match msg_id {
        crate::COMMAND_PTZ => Some(PtzResponseKind::Move),
        crate::COMMAND_PTZ_PRESET_LIST => Some(PtzResponseKind::PresetList),
        crate::COMMAND_PTZ_PRESET => Some(PtzResponseKind::Preset),
        crate::COMMAND_PTZ_ZOOM_FOCUS => Some(PtzResponseKind::ZoomFocus),
        crate::COMMAND_START_ZOOM_FOCUS => Some(PtzResponseKind::StartZoomFocus),
        crate::COMMAND_PTZ_GUARD => Some(PtzResponseKind::Guard),
        _ => None,
    }
}

pub fn build_request(cmd: &PtzCommand, buf: &mut [u8]) -> Result<(PacketHeader, usize), BcError> {
    match cmd {
        PtzCommand::Move {
            channel,
            direction,
            speed,
        } => {
            let clamped = (*speed).clamp(1, 64);
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PtzControl", "1.1");
                b.u8_element("channelId", *channel);
                b.text_element("command", direction.as_str());
                b.u8_element("speed", clamped);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PTZ, len), len))
        }
        PtzCommand::Stop { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PtzControl", "1.1");
                b.u8_element("channelId", *channel);
                b.text_element("command", "stop");
                b.u8_element("speed", 0);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PTZ, len), len))
        }
        PtzCommand::PresetList { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PtzPresetList", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PTZ_PRESET_LIST, len), len))
        }
        PtzCommand::PresetGoto { channel, preset_id } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PtzPreset", "1.1");
                b.u8_element("channelId", *channel);
                b.text_element("command", "goto");
                b.u32_element("presetId", *preset_id);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PTZ_PRESET, len), len))
        }
        PtzCommand::PresetSave {
            channel,
            preset_id,
            name,
        } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PtzPreset", "1.1");
                b.u8_element("channelId", *channel);
                b.text_element("command", "save");
                b.u32_element("presetId", *preset_id);
                b.text_element("name", name.as_str());
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PTZ_PRESET, len), len))
        }
        PtzCommand::PresetDelete { channel, preset_id } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PtzPreset", "1.1");
                b.u8_element("channelId", *channel);
                b.text_element("command", "delete");
                b.u32_element("presetId", *preset_id);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PTZ_PRESET, len), len))
        }
        PtzCommand::GetZoomFocus { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PtzZoomFocus", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PTZ_ZOOM_FOCUS, len), len))
        }
        PtzCommand::StartZoomFocus { channel, operation } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("StartZoomFocus", "1.1");
                b.u8_element("channelId", *channel);
                b.text_element("command", operation.as_str());
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_START_ZOOM_FOCUS, len), len))
        }
        PtzCommand::GetGuard { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PtzGuard", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PTZ_GUARD, len), len))
        }
        PtzCommand::SetGuard(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PtzGuard", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.u32_element("presetId", cfg.preset_id);
                b.u32_element("waitTime", cfg.wait_time);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PTZ_GUARD, len), len))
        }
    }
}

pub fn parse_response(kind: PtzResponseKind, body: &[u8]) -> Result<PtzEvent, BcError> {
    match kind {
        PtzResponseKind::Move => Ok(PtzEvent::MoveAck),
        PtzResponseKind::Preset => Ok(PtzEvent::PresetAck),
        PtzResponseKind::PresetList => {
            let mut presets = Vec::new();
            let mut current_id: Option<u32> = None;
            let mut current_name = ArrayString::<NAME_CAP>::new();

            xml::parse_xml(body, |name, text| match name {
                "presetId" | "id" => {
                    // Flush previous preset if any
                    if let Some(id) = current_id.take() {
                        presets.push(PresetPosition {
                            id,
                            name: current_name,
                        });
                        current_name = ArrayString::new();
                    }
                    if let Ok(v) = text.parse::<u32>() {
                        current_id = Some(v);
                    }
                }
                "name" => {
                    let _ = ArrayString::try_from(text).map(|s| current_name = s);
                }
                _ => {}
            })?;

            // Flush last preset
            if let Some(id) = current_id {
                presets.push(PresetPosition {
                    id,
                    name: current_name,
                });
            }

            Ok(PtzEvent::PresetList(presets))
        }
        PtzResponseKind::ZoomFocus => {
            let mut info = ZoomFocusInfo { zoom: 0, focus: 0 };
            xml::parse_xml(body, |name, text| {
                if let Ok(v) = text.parse::<u32>() {
                    match name {
                        "zoom" | "zoomPos" => info.zoom = v,
                        "focus" | "focusPos" => info.focus = v,
                        _ => {}
                    }
                }
            })?;
            Ok(PtzEvent::ZoomFocus(info))
        }
        PtzResponseKind::StartZoomFocus => Ok(PtzEvent::ZoomFocusAck),
        PtzResponseKind::Guard => {
            let mut cfg = PtzGuardConfig {
                channel: 0,
                enabled: false,
                preset_id: 0,
                wait_time: 0,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cfg.channel = v;
                    }
                }
                "enable" => cfg.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                "presetId" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cfg.preset_id = v;
                    }
                }
                "waitTime" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cfg.wait_time = v;
                    }
                }
                _ => {}
            })?;
            Ok(PtzEvent::Guard(cfg))
        }
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
    fn classify_ptz_ids() {
        assert!(matches!(
            classify_response(crate::COMMAND_PTZ),
            Some(PtzResponseKind::Move)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_PTZ_PRESET),
            Some(PtzResponseKind::Preset)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_PTZ_PRESET_LIST),
            Some(PtzResponseKind::PresetList)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_PTZ_GUARD),
            Some(PtzResponseKind::Guard)
        ));
        assert!(classify_response(999).is_none());
    }

    #[test]
    fn build_move_left() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &PtzCommand::Move {
                channel: 0,
                direction: PtzDirection::Left,
                speed: 32,
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_PTZ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<PtzControl"));
        assert!(xml.contains("<command>left</command>"));
        assert!(xml.contains("<speed>32</speed>"));
    }

    #[test]
    fn build_move_all_directions() {
        let directions = [
            (PtzDirection::Left, "left"),
            (PtzDirection::Right, "right"),
            (PtzDirection::Up, "up"),
            (PtzDirection::Down, "down"),
            (PtzDirection::LeftUp, "leftUp"),
            (PtzDirection::RightDown, "rightDown"),
            (PtzDirection::ZoomIn, "zoomIn"),
            (PtzDirection::ZoomOut, "zoomOut"),
            (PtzDirection::FocusNear, "focusNear"),
            (PtzDirection::FocusFar, "focusFar"),
        ];

        for (dir, expected) in directions {
            let mut buf = [0u8; 512];
            let (_, len) = build_request(
                &PtzCommand::Move {
                    channel: 0,
                    direction: dir,
                    speed: 10,
                },
                &mut buf,
            )
            .unwrap();
            let xml = std::str::from_utf8(&buf[..len]).unwrap();
            assert!(
                xml.contains(&format!("<command>{expected}</command>")),
                "direction {dir:?} should produce '{expected}'"
            );
        }
    }

    #[test]
    fn speed_clamped_to_range() {
        let mut buf = [0u8; 512];

        // speed 0 → clamped to 1
        let (_, len) = build_request(
            &PtzCommand::Move {
                channel: 0,
                direction: PtzDirection::Left,
                speed: 0,
            },
            &mut buf,
        )
        .unwrap();
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<speed>1</speed>"));

        // speed 255 → clamped to 64
        let (_, len) = build_request(
            &PtzCommand::Move {
                channel: 0,
                direction: PtzDirection::Right,
                speed: 255,
            },
            &mut buf,
        )
        .unwrap();
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<speed>64</speed>"));
    }

    #[test]
    fn build_stop() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&PtzCommand::Stop { channel: 0 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_PTZ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<command>stop</command>"));
        assert!(xml.contains("<speed>0</speed>"));
    }

    #[test]
    fn build_preset_goto() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &PtzCommand::PresetGoto {
                channel: 0,
                preset_id: 5,
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_PTZ_PRESET);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<command>goto</command>"));
        assert!(xml.contains("<presetId>5</presetId>"));
    }

    #[test]
    fn build_preset_save() {
        let mut buf = [0u8; 1024];
        let (hdr, len) = build_request(
            &PtzCommand::PresetSave {
                channel: 0,
                preset_id: 3,
                name: ArrayString::try_from("Gate View").unwrap(),
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_PTZ_PRESET);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<command>save</command>"));
        assert!(xml.contains("<name>Gate View</name>"));
    }

    #[test]
    fn build_get_zoom_focus() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&PtzCommand::GetZoomFocus { channel: 0 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_PTZ_ZOOM_FOCUS);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<PtzZoomFocus"));
    }

    #[test]
    fn build_start_zoom_focus() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &PtzCommand::StartZoomFocus {
                channel: 0,
                operation: ZoomFocusOp::ZoomIn,
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_START_ZOOM_FOCUS);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<command>zoomIn</command>"));
    }

    #[test]
    fn build_set_guard() {
        let mut buf = [0u8; 1024];
        let cfg = PtzGuardConfig {
            channel: 0,
            enabled: true,
            preset_id: 1,
            wait_time: 30,
        };
        let (hdr, len) = build_request(&PtzCommand::SetGuard(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_PTZ_GUARD);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<enable>1</enable>"));
        assert!(xml.contains("<presetId>1</presetId>"));
        assert!(xml.contains("<waitTime>30</waitTime>"));
    }

    #[test]
    fn parse_preset_list_response() {
        let xml = b"<body>\
            <PtzPresetList version=\"1.1\">\
                <presetId>1</presetId>\
                <name>Front Gate</name>\
                <presetId>2</presetId>\
                <name>Backyard</name>\
                <presetId>3</presetId>\
                <name>Driveway</name>\
            </PtzPresetList>\
        </body>";
        let event = parse_response(PtzResponseKind::PresetList, xml).unwrap();
        match event {
            PtzEvent::PresetList(list) => {
                assert_eq!(list.len(), 3);
                assert_eq!(list[0].id, 1);
                assert_eq!(list[0].name.as_str(), "Front Gate");
                assert_eq!(list[1].id, 2);
                assert_eq!(list[1].name.as_str(), "Backyard");
                assert_eq!(list[2].id, 3);
                assert_eq!(list[2].name.as_str(), "Driveway");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_zoom_focus_response() {
        let xml = b"<body>\
            <PtzZoomFocus version=\"1.1\">\
                <zoom>500</zoom>\
                <focus>300</focus>\
            </PtzZoomFocus>\
        </body>";
        let event = parse_response(PtzResponseKind::ZoomFocus, xml).unwrap();
        match event {
            PtzEvent::ZoomFocus(info) => {
                assert_eq!(info.zoom, 500);
                assert_eq!(info.focus, 300);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_guard_response() {
        let xml = b"<body>\
            <PtzGuard version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
                <presetId>2</presetId>\
                <waitTime>60</waitTime>\
            </PtzGuard>\
        </body>";
        let event = parse_response(PtzResponseKind::Guard, xml).unwrap();
        match event {
            PtzEvent::Guard(cfg) => {
                assert!(cfg.enabled);
                assert_eq!(cfg.preset_id, 2);
                assert_eq!(cfg.wait_time, 60);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_move_ack() {
        let xml = b"<body><PtzControl version=\"1.1\"></PtzControl></body>";
        let event = parse_response(PtzResponseKind::Move, xml).unwrap();
        assert!(matches!(event, PtzEvent::MoveAck));
    }
}
