//! Alarm & detection commands.
//!
//! Provides builders and parsers for motion alarm, motion detection config,
//! RF alarm, PIR, AI config/alarm, audio task, and unsolicited alarm events.

use crate::{error::BcError, header::PacketHeader, magic::*, xml};
use arrayvec::{ArrayString, ArrayVec};

const ALARM_EVENT_CAP: usize = 16;
const EVENT_VALUE_CAP: usize = 64;

/// Alarm command (client → camera).
#[derive(Debug, Clone, Copy)]
pub enum AlarmCommand {
    /// Enable motion alarm push notifications (ID 31).
    StartMotionAlarm { channel: u8 },
    /// Read motion detection config (ID 46).
    GetMotionDetect { channel: u8 },
    /// Write motion detection config (ID 47).
    SetMotionDetect(MotionDetectConfig),
    /// Read RF alarm info (ID 133).
    GetRfAlarm { channel: u8 },
    /// Write RF alarm config (ID 204).
    SetRfAlarmCfg(RfAlarmConfig),
    /// Read PIR sensor config (ID 212).
    GetPir { channel: u8 },
    /// Write PIR sensor config (ID 213).
    SetPir(PirConfig),
    /// Read AI detection config (ID 299).
    GetAiCfg { channel: u8 },
    /// Read AI alarm config (ID 342).
    GetAiAlarm { channel: u8 },
    /// Write AI alarm config (ID 343).
    SetAiAlarm(AiAlarmConfig),
    /// Read audio task config (ID 232).
    GetAudioTask { channel: u8 },
}

/// Alarm event (camera → client).
#[derive(Debug, Clone)]
pub enum AlertEvent {
    /// Ack for StartMotionAlarm.
    MotionAlarmStarted,
    /// Unsolicited alarm event push from camera (ID 33).
    AlarmEventList(Box<AlarmEventList>),
    /// Motion detection config response.
    MotionDetect(MotionDetectConfig),
    /// Ack for SetMotionDetect.
    MotionDetectAck,
    /// RF alarm info response.
    RfAlarm(RfAlarmConfig),
    /// Ack for SetRfAlarmCfg.
    RfAlarmCfgAck,
    /// PIR config response.
    Pir(PirConfig),
    /// Ack for SetPir.
    PirAck,
    /// AI detection config response.
    AiCfg(AiCfgData),
    /// AI alarm config response.
    AiAlarm(AiAlarmConfig),
    /// Ack for SetAiAlarm.
    AiAlarmAck,
    /// Audio task config response.
    AudioTask(AudioTaskData),
    /// Unsolicited auto-tracking coordinate push from camera (ID 723).
    CoordinateInfo(CoordinateData),
}

/// Motion detection configuration.
#[derive(Debug, Clone, Copy)]
pub struct MotionDetectConfig {
    pub channel: u8,
    pub enabled: bool,
    pub sensitivity: u32,
}

/// RF alarm configuration.
#[derive(Debug, Clone, Copy)]
pub struct RfAlarmConfig {
    pub channel: u8,
    pub enabled: bool,
}

/// PIR sensor configuration.
#[derive(Debug, Clone, Copy)]
pub struct PirConfig {
    pub channel: u8,
    pub enabled: bool,
    pub sensitivity: u32,
}

/// AI detection config (tracking settings).
#[derive(Debug, Clone, Copy)]
pub struct AiCfgData {
    pub channel: u8,
    pub track_enabled: bool,
    pub sensitivity: u32,
}

/// AI alarm configuration (per-object-type enable flags).
#[derive(Debug, Clone, Copy)]
pub struct AiAlarmConfig {
    pub channel: u8,
    pub person: bool,
    pub vehicle: bool,
    pub dog_cat: bool,
    pub face: bool,
    pub package: bool,
}

/// Alarm events from one unsolicited push (ID 33).
#[derive(Debug, Clone, Default)]
pub struct AlarmEventList {
    /// Events represented in camera payload order.
    pub events: ArrayVec<AlarmEventData, ALARM_EVENT_CAP>,
}

/// One camera alarm event from an unsolicited push (ID 33).
#[derive(Debug, Clone, Default)]
pub struct AlarmEventData {
    pub channel: u8,
    /// Camera-reported alarm category in legacy payloads.
    pub alarm_type: ArrayString<EVENT_VALUE_CAP>,
    /// Camera-reported motion status, typically `MD` or `none`.
    pub status: ArrayString<EVENT_VALUE_CAP>,
    /// Camera-reported AI types, typically `people`, `vehicle`, or `none`.
    pub ai_types: ArrayString<EVENT_VALUE_CAP>,
    pub recording: Option<bool>,
    pub timestamp: Option<u64>,
}

impl AlarmEventData {
    pub fn is_active(&self) -> bool {
        has_active_alarm_value(self.ai_types.as_str())
            || (!self.status.is_empty() && has_active_alarm_value(self.status.as_str()))
            || (self.status.is_empty() && has_active_alarm_value(self.alarm_type.as_str()))
    }
}

fn has_active_alarm_value(value: &str) -> bool {
    value.split(',').map(str::trim).any(|value| {
        !value.is_empty()
            && !value.eq_ignore_ascii_case("none")
            && value != "0"
            && !value.eq_ignore_ascii_case("false")
    })
}

/// Audio task configuration.
#[derive(Debug, Clone, Copy)]
pub struct AudioTaskData {
    pub channel: u8,
    pub enabled: bool,
}

/// Auto-tracking coordinate push data (ID 723).
#[derive(Debug, Clone, Copy)]
pub struct CoordinateData {
    pub channel: u8,
    pub x: u32,
    pub y: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum AlarmResponseKind {
    MotionAlarmStarted,
    AlarmEventList,
    MotionDetectRead,
    MotionDetectWrite,
    RfAlarm,
    RfAlarmCfgWrite,
    PirRead,
    PirWrite,
    AiCfg,
    AiAlarmRead,
    AiAlarmWrite,
    AudioTask,
    CoordinateInfo,
}

/// Classify an incoming msg_id as an alarm response.
pub const fn classify_response(msg_id: u32) -> Option<AlarmResponseKind> {
    match msg_id {
        crate::COMMAND_START_MOTION_ALARM => Some(AlarmResponseKind::MotionAlarmStarted),
        crate::COMMAND_ALARM_EVENT_LIST => Some(AlarmResponseKind::AlarmEventList),
        crate::COMMAND_MOTION_DETECT_READ => Some(AlarmResponseKind::MotionDetectRead),
        crate::COMMAND_MOTION_DETECT_WRITE => Some(AlarmResponseKind::MotionDetectWrite),
        crate::COMMAND_RF_ALARM => Some(AlarmResponseKind::RfAlarm),
        crate::COMMAND_RF_ALARM_CFG_WRITE => Some(AlarmResponseKind::RfAlarmCfgWrite),
        crate::COMMAND_PIR_READ => Some(AlarmResponseKind::PirRead),
        crate::COMMAND_PIR_WRITE => Some(AlarmResponseKind::PirWrite),
        crate::COMMAND_AI_CFG_READ => Some(AlarmResponseKind::AiCfg),
        crate::COMMAND_AI_ALARM_READ => Some(AlarmResponseKind::AiAlarmRead),
        crate::COMMAND_AI_ALARM_WRITE => Some(AlarmResponseKind::AiAlarmWrite),
        crate::COMMAND_AUDIO_TASK_READ => Some(AlarmResponseKind::AudioTask),
        crate::COMMAND_COORDINATE_INFO => Some(AlarmResponseKind::CoordinateInfo),
        _ => None,
    }
}

pub fn build_request(cmd: &AlarmCommand, buf: &mut [u8]) -> Result<(PacketHeader, usize), BcError> {
    match cmd {
        AlarmCommand::StartMotionAlarm { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("StartMotionAlarm", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_START_MOTION_ALARM, len), len))
        }
        AlarmCommand::GetMotionDetect { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("MotionDetect", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_MOTION_DETECT_READ, len), len))
        }
        AlarmCommand::SetMotionDetect(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("MotionDetect", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.u32_element("sensitivity", cfg.sensitivity);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_MOTION_DETECT_WRITE, len), len))
        }
        AlarmCommand::GetRfAlarm { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RfAlarm", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RF_ALARM, len), len))
        }
        AlarmCommand::SetRfAlarmCfg(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RfAlarmCfg", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RF_ALARM_CFG_WRITE, len), len))
        }
        AlarmCommand::GetPir { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PirInfo", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PIR_READ, len), len))
        }
        AlarmCommand::SetPir(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PirInfo", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.u32_element("sensitivity", cfg.sensitivity);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PIR_WRITE, len), len))
        }
        AlarmCommand::GetAiCfg { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("AiCfg", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_AI_CFG_READ, len), len))
        }
        AlarmCommand::GetAiAlarm { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("AiAlarm", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_AI_ALARM_READ, len), len))
        }
        AlarmCommand::SetAiAlarm(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("AiAlarm", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("person", if cfg.person { "1" } else { "0" });
                b.text_element("vehicle", if cfg.vehicle { "1" } else { "0" });
                b.text_element("dogCat", if cfg.dog_cat { "1" } else { "0" });
                b.text_element("face", if cfg.face { "1" } else { "0" });
                b.text_element("package", if cfg.package { "1" } else { "0" });
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_AI_ALARM_WRITE, len), len))
        }
        AlarmCommand::GetAudioTask { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("AudioTask", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_AUDIO_TASK_READ, len), len))
        }
    }
}

pub fn parse_response(kind: AlarmResponseKind, body: &[u8]) -> Result<AlertEvent, BcError> {
    match kind {
        AlarmResponseKind::MotionAlarmStarted => Ok(AlertEvent::MotionAlarmStarted),
        AlarmResponseKind::AlarmEventList => {
            let mut events = AlarmEventList::default();
            let mut current = None;
            let mut flat_event = AlarmEventData::default();
            let mut flat_event_seen = false;
            let mut too_many_events = false;

            xml::visit_xml(body, |event| match event {
                xml::XmlVisit::Start("AlarmEvent") => {
                    current = Some(AlarmEventData::default());
                }
                xml::XmlVisit::Text { name, text } => {
                    if let Some(alarm) = current.as_mut() {
                        update_alarm_event(alarm, name, text);
                    } else if update_alarm_event(&mut flat_event, name, text) {
                        flat_event_seen = true;
                    }
                }
                xml::XmlVisit::End("AlarmEvent") => {
                    if let Some(alarm) = current.take()
                        && events.events.try_push(alarm).is_err()
                    {
                        too_many_events = true;
                    }
                }
                _ => {}
            })?;

            if too_many_events {
                return Err(BcError::Protocol("alarm event list exceeds capacity"));
            }
            if events.events.is_empty() && flat_event_seen {
                events
                    .events
                    .try_push(flat_event)
                    .map_err(|_| BcError::Protocol("alarm event list exceeds capacity"))?;
            }
            Ok(AlertEvent::AlarmEventList(Box::new(events)))
        }
        AlarmResponseKind::MotionDetectRead => {
            let mut cfg = MotionDetectConfig {
                channel: 0,
                enabled: false,
                sensitivity: 0,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cfg.channel = v;
                    }
                }
                "enable" => cfg.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                "sensitivity" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cfg.sensitivity = v;
                    }
                }
                _ => {}
            })?;
            Ok(AlertEvent::MotionDetect(cfg))
        }
        AlarmResponseKind::MotionDetectWrite => Ok(AlertEvent::MotionDetectAck),
        AlarmResponseKind::RfAlarm => {
            let mut cfg = RfAlarmConfig {
                channel: 0,
                enabled: false,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cfg.channel = v;
                    }
                }
                "enable" => cfg.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                _ => {}
            })?;
            Ok(AlertEvent::RfAlarm(cfg))
        }
        AlarmResponseKind::RfAlarmCfgWrite => Ok(AlertEvent::RfAlarmCfgAck),
        AlarmResponseKind::PirRead => {
            let mut cfg = PirConfig {
                channel: 0,
                enabled: false,
                sensitivity: 0,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cfg.channel = v;
                    }
                }
                "enable" => cfg.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                "sensitivity" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cfg.sensitivity = v;
                    }
                }
                _ => {}
            })?;
            Ok(AlertEvent::Pir(cfg))
        }
        AlarmResponseKind::PirWrite => Ok(AlertEvent::PirAck),
        AlarmResponseKind::AiCfg => {
            let mut data = AiCfgData {
                channel: 0,
                track_enabled: false,
                sensitivity: 0,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        data.channel = v;
                    }
                }
                "trackEnable" | "trackEnabled" => {
                    data.track_enabled = text == "1" || text.eq_ignore_ascii_case("true");
                }
                "sensitivity" => {
                    if let Ok(v) = text.parse::<u32>() {
                        data.sensitivity = v;
                    }
                }
                _ => {}
            })?;
            Ok(AlertEvent::AiCfg(data))
        }
        AlarmResponseKind::AiAlarmRead => {
            let mut cfg = AiAlarmConfig {
                channel: 0,
                person: false,
                vehicle: false,
                dog_cat: false,
                face: false,
                package: false,
            };
            xml::parse_xml(body, |name, text| {
                let flag = text == "1" || text.eq_ignore_ascii_case("true");
                match name {
                    "channelId" => {
                        if let Ok(v) = text.parse::<u8>() {
                            cfg.channel = v;
                        }
                    }
                    "person" | "people" => cfg.person = flag,
                    "vehicle" | "car" => cfg.vehicle = flag,
                    "dogCat" | "dog_cat" | "animal" => cfg.dog_cat = flag,
                    "face" => cfg.face = flag,
                    "package" => cfg.package = flag,
                    _ => {}
                }
            })?;
            Ok(AlertEvent::AiAlarm(cfg))
        }
        AlarmResponseKind::AiAlarmWrite => Ok(AlertEvent::AiAlarmAck),
        AlarmResponseKind::AudioTask => {
            let mut data = AudioTaskData {
                channel: 0,
                enabled: false,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        data.channel = v;
                    }
                }
                "enable" => data.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                _ => {}
            })?;
            Ok(AlertEvent::AudioTask(data))
        }
        AlarmResponseKind::CoordinateInfo => {
            let mut data = CoordinateData {
                channel: 0,
                x: 0,
                y: 0,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        data.channel = v;
                    }
                }
                "x" | "posX" => {
                    if let Ok(v) = text.parse::<u32>() {
                        data.x = v;
                    }
                }
                "y" | "posY" => {
                    if let Ok(v) = text.parse::<u32>() {
                        data.y = v;
                    }
                }
                _ => {}
            })?;
            Ok(AlertEvent::CoordinateInfo(data))
        }
    }
}

fn update_alarm_event(event: &mut AlarmEventData, name: &str, text: &str) -> bool {
    let text = text.trim();
    match name {
        "channelId" => {
            if let Ok(channel) = text.parse::<u8>() {
                event.channel = channel;
            }
            true
        }
        "alarmType" | "type" => {
            if let Ok(alarm_type) = ArrayString::<EVENT_VALUE_CAP>::try_from(text) {
                event.alarm_type = alarm_type;
            }
            true
        }
        "status" => {
            if let Ok(status) = ArrayString::<EVENT_VALUE_CAP>::try_from(text) {
                event.status = status;
            }
            true
        }
        "AItype" | "aiType" => {
            if let Ok(ai_types) = ArrayString::<EVENT_VALUE_CAP>::try_from(text) {
                event.ai_types = ai_types;
            }
            true
        }
        "recording" => {
            event.recording = match text {
                "0" | "false" | "False" => Some(false),
                "1" | "true" | "True" => Some(true),
                _ => None,
            };
            true
        }
        "timeStamp" | "timestamp" => {
            event.timestamp = text.parse::<u64>().ok();
            true
        }
        _ => false,
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
    fn classify_alarm_ids() {
        assert!(matches!(
            classify_response(crate::COMMAND_START_MOTION_ALARM),
            Some(AlarmResponseKind::MotionAlarmStarted)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_ALARM_EVENT_LIST),
            Some(AlarmResponseKind::AlarmEventList)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_MOTION_DETECT_READ),
            Some(AlarmResponseKind::MotionDetectRead)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_MOTION_DETECT_WRITE),
            Some(AlarmResponseKind::MotionDetectWrite)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_AI_CFG_READ),
            Some(AlarmResponseKind::AiCfg)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_AI_ALARM_READ),
            Some(AlarmResponseKind::AiAlarmRead)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_COORDINATE_INFO),
            Some(AlarmResponseKind::CoordinateInfo)
        ));
        assert!(classify_response(999).is_none());
    }

    #[test]
    fn build_start_motion_alarm() {
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&AlarmCommand::StartMotionAlarm { channel: 0 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_START_MOTION_ALARM);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<StartMotionAlarm"));
        assert!(xml.contains("<channelId>0</channelId>"));
    }

    #[test]
    fn build_set_motion_detect() {
        let cfg = MotionDetectConfig {
            channel: 0,
            enabled: true,
            sensitivity: 50,
        };
        let mut buf = [0u8; 1024];
        let (hdr, len) = build_request(&AlarmCommand::SetMotionDetect(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_MOTION_DETECT_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<enable>1</enable>"));
        assert!(xml.contains("<sensitivity>50</sensitivity>"));
    }

    #[test]
    fn build_set_rf_alarm_cfg() {
        let cfg = RfAlarmConfig {
            channel: 1,
            enabled: false,
        };
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&AlarmCommand::SetRfAlarmCfg(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_RF_ALARM_CFG_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<enable>0</enable>"));
        assert!(xml.contains("<channelId>1</channelId>"));
    }

    #[test]
    fn build_set_pir() {
        let cfg = PirConfig {
            channel: 0,
            enabled: true,
            sensitivity: 75,
        };
        let mut buf = [0u8; 1024];
        let (hdr, len) = build_request(&AlarmCommand::SetPir(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_PIR_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<enable>1</enable>"));
        assert!(xml.contains("<sensitivity>75</sensitivity>"));
    }

    #[test]
    fn build_set_ai_alarm() {
        let cfg = AiAlarmConfig {
            channel: 0,
            person: true,
            vehicle: false,
            dog_cat: true,
            face: false,
            package: true,
        };
        let mut buf = [0u8; 1024];
        let (hdr, len) = build_request(&AlarmCommand::SetAiAlarm(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_AI_ALARM_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<person>1</person>"));
        assert!(xml.contains("<vehicle>0</vehicle>"));
        assert!(xml.contains("<dogCat>1</dogCat>"));
        assert!(xml.contains("<face>0</face>"));
        assert!(xml.contains("<package>1</package>"));
    }

    #[test]
    fn build_get_ai_cfg() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&AlarmCommand::GetAiCfg { channel: 0 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_AI_CFG_READ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<AiCfg"));
    }

    #[test]
    fn build_get_audio_task() {
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&AlarmCommand::GetAudioTask { channel: 0 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_AUDIO_TASK_READ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<AudioTask"));
    }

    #[test]
    fn parse_alarm_event_list() {
        let xml = b"<body>\
            <AlarmEventList version=\"1.1\">\
                <AlarmEvent version=\"1.1\">\
                    <channelId>0</channelId>\
                    <status>MD</status>\
                    <AItype>people,vehicle</AItype>\
                </AlarmEvent>\
            </AlarmEventList>\
        </body>";
        let event = parse_response(AlarmResponseKind::AlarmEventList, xml).unwrap();
        match event {
            AlertEvent::AlarmEventList(events) => {
                assert_eq!(events.events.len(), 1);
                assert_eq!(events.events[0].channel, 0);
                assert_eq!(events.events[0].status.as_str(), "MD");
                assert_eq!(events.events[0].ai_types.as_str(), "people,vehicle");
                assert!(events.events[0].is_active());
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_motion_detect_response() {
        let xml = b"<body>\
            <MotionDetect version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
                <sensitivity>60</sensitivity>\
            </MotionDetect>\
        </body>";
        let event = parse_response(AlarmResponseKind::MotionDetectRead, xml).unwrap();
        match event {
            AlertEvent::MotionDetect(cfg) => {
                assert!(cfg.enabled);
                assert_eq!(cfg.sensitivity, 60);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_ai_alarm_response() {
        let xml = b"<body>\
            <AiAlarm version=\"1.1\">\
                <channelId>0</channelId>\
                <person>1</person>\
                <vehicle>0</vehicle>\
                <dogCat>1</dogCat>\
                <face>0</face>\
                <package>1</package>\
            </AiAlarm>\
        </body>";
        let event = parse_response(AlarmResponseKind::AiAlarmRead, xml).unwrap();
        match event {
            AlertEvent::AiAlarm(cfg) => {
                assert!(cfg.person);
                assert!(!cfg.vehicle);
                assert!(cfg.dog_cat);
                assert!(!cfg.face);
                assert!(cfg.package);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_ai_cfg_response() {
        let xml = b"<body>\
            <AiCfg version=\"1.1\">\
                <channelId>0</channelId>\
                <trackEnable>1</trackEnable>\
                <sensitivity>80</sensitivity>\
            </AiCfg>\
        </body>";
        let event = parse_response(AlarmResponseKind::AiCfg, xml).unwrap();
        match event {
            AlertEvent::AiCfg(data) => {
                assert!(data.track_enabled);
                assert_eq!(data.sensitivity, 80);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_pir_response() {
        let xml = b"<body>\
            <PirInfo version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
                <sensitivity>90</sensitivity>\
            </PirInfo>\
        </body>";
        let event = parse_response(AlarmResponseKind::PirRead, xml).unwrap();
        match event {
            AlertEvent::Pir(cfg) => {
                assert!(cfg.enabled);
                assert_eq!(cfg.sensitivity, 90);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_coordinate_info() {
        let xml = b"<body>\
            <CoordinateInfo version=\"1.1\">\
                <channelId>0</channelId>\
                <x>320</x>\
                <y>240</y>\
            </CoordinateInfo>\
        </body>";
        let event = parse_response(AlarmResponseKind::CoordinateInfo, xml).unwrap();
        match event {
            AlertEvent::CoordinateInfo(data) => {
                assert_eq!(data.x, 320);
                assert_eq!(data.y, 240);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_motion_alarm_started_ack() {
        let xml = b"<body><StartMotionAlarm version=\"1.1\"></StartMotionAlarm></body>";
        let event = parse_response(AlarmResponseKind::MotionAlarmStarted, xml).unwrap();
        assert!(matches!(event, AlertEvent::MotionAlarmStarted));
    }

    #[test]
    fn parse_audio_task_response() {
        let xml = b"<body>\
            <AudioTask version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>0</enable>\
            </AudioTask>\
        </body>";
        let event = parse_response(AlarmResponseKind::AudioTask, xml).unwrap();
        match event {
            AlertEvent::AudioTask(data) => {
                assert!(!data.enabled);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_rf_alarm_response() {
        let xml = b"<body>\
            <RfAlarm version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
            </RfAlarm>\
        </body>";
        let event = parse_response(AlarmResponseKind::RfAlarm, xml).unwrap();
        match event {
            AlertEvent::RfAlarm(cfg) => {
                assert!(cfg.enabled);
            }
            _ => panic!("wrong event"),
        }
    }
}
