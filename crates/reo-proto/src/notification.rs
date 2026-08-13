//! Notification & output device commands.
//!
//! Provides builders and parsers for email, push notifications, LED state,
//! battery info, floodlight, siren, and audio play info.

use crate::{error::BcError, header::PacketHeader, magic::*, xml};
use arrayvec::ArrayString;

/// Email configuration.
#[derive(Debug, Clone)]
pub struct EmailConfig {
    pub channel: u8,
    pub enabled: bool,
    pub smtp_server: ArrayString<64>,
    pub smtp_port: u32,
    pub sender: ArrayString<64>,
    pub ssl: bool,
}

/// Email task configuration.
#[derive(Debug, Clone, Copy)]
pub struct EmailTaskConfig {
    pub channel: u8,
    pub enabled: bool,
}

/// Push notification info.
#[derive(Debug, Clone, Copy)]
pub struct PushInfoData {
    pub channel: u8,
    pub enabled: bool,
}

/// Push task data.
#[derive(Debug, Clone, Copy)]
pub struct PushTaskData {
    pub channel: u8,
    pub enabled: bool,
}

/// LED state configuration.
#[derive(Debug, Clone)]
pub struct LedStateConfig {
    pub channel: u8,
    pub enabled: bool,
    pub state: ArrayString<32>,
}

/// Battery info data.
#[derive(Debug, Clone, Copy)]
pub struct BatteryInfoData {
    pub channel: u8,
    pub capacity: u32,
    pub temperature: i32,
    pub charging: bool,
}

/// Floodlight manual control.
#[derive(Debug, Clone, Copy)]
pub struct ManualLightState {
    pub channel: u8,
    pub enabled: bool,
}

/// Floodlight task configuration.
#[derive(Debug, Clone, Copy)]
pub struct FloodlightTaskConfig {
    pub channel: u8,
    pub enabled: bool,
    pub brightness: u32,
}

/// Floodlight status data.
#[derive(Debug, Clone, Copy)]
pub struct FloodlightStatusData {
    pub channel: u8,
    pub enabled: bool,
}

/// Audio play info data.
#[derive(Debug, Clone, Copy)]
pub struct AudioPlayInfoData {
    pub channel: u8,
    pub enabled: bool,
}

/// Notification command (client → camera).
#[derive(Debug, Clone)]
pub enum NotificationCommand {
    /// Read email config (ID 42).
    GetEmail { channel: u8 },
    /// Write email config (ID 43).
    SetEmail(EmailConfig),
    /// Test email delivery (ID 141).
    TestEmail,
    /// Read email task (ID 217).
    GetEmailTask { channel: u8 },
    /// Write email task (ID 216).
    SetEmailTask(EmailTaskConfig),
    /// Read push notification info (ID 124).
    GetPushInfo { channel: u8 },
    /// Read push task config (ID 219).
    GetPushTask { channel: u8 },
    /// Read LED state (ID 208).
    GetLedState { channel: u8 },
    /// Write LED state (ID 209).
    SetLedState(LedStateConfig),
    /// Get battery list (ID 252).
    GetBatteryList,
    /// Get battery info for channel (ID 253).
    GetBatteryInfo { channel: u8 },
    /// Set floodlight on/off (ID 288).
    SetFloodlight(ManualLightState),
    /// Get floodlight task config (ID 290).
    GetFloodlightTask { channel: u8 },
    /// Write floodlight task config (ID 438).
    SetFloodlightTask(FloodlightTaskConfig),
    /// Get floodlight status list (ID 291).
    GetFloodlightStatusList,
    /// Siren control (ID 547).
    SirenControl { channel: u8, enabled: bool },
    /// Get audio play info (ID 264).
    GetAudioPlayInfo { channel: u8 },
}

/// Notification event (camera → client).
#[derive(Debug, Clone)]
pub enum NotificationEvent {
    /// Email config response.
    Email(EmailConfig),
    /// Email config written ack.
    EmailAck,
    /// Email test result.
    EmailTestResult { success: bool },
    /// Email task response.
    EmailTask(EmailTaskConfig),
    /// Email task written ack.
    EmailTaskAck,
    /// Push notification info response.
    PushInfo(PushInfoData),
    /// Push task response.
    PushTask(PushTaskData),
    /// LED state response.
    LedState(LedStateConfig),
    /// LED state written ack.
    LedStateAck,
    /// Battery list response.
    BatteryList(Vec<BatteryInfoData>),
    /// Battery info response.
    BatteryInfo(BatteryInfoData),
    /// Floodlight manual ack.
    FloodlightAck,
    /// Floodlight task response.
    FloodlightTask(FloodlightTaskConfig),
    /// Floodlight task written ack.
    FloodlightTaskAck,
    /// Floodlight status list response.
    FloodlightStatusList(Vec<FloodlightStatusData>),
    /// Siren control ack.
    SirenControlAck,
    /// Audio play info response.
    AudioPlayInfo(AudioPlayInfoData),
}

#[derive(Debug, Clone, Copy)]
pub enum NotificationResponseKind {
    EmailRead,
    EmailWrite,
    EmailTest,
    EmailTaskRead,
    EmailTaskWrite,
    PushInfo,
    PushTask,
    LedRead,
    LedWrite,
    BatteryList,
    BatteryInfo,
    Floodlight,
    FloodlightTaskRead,
    FloodlightTaskWrite,
    FloodlightStatusList,
    SirenControl,
    AudioPlayInfo,
}

/// Classify an incoming msg_id as a notification response.
pub const fn classify_response(msg_id: u32) -> Option<NotificationResponseKind> {
    match msg_id {
        crate::COMMAND_EMAIL_READ => Some(NotificationResponseKind::EmailRead),
        crate::COMMAND_EMAIL_WRITE => Some(NotificationResponseKind::EmailWrite),
        crate::COMMAND_EMAIL_TEST => Some(NotificationResponseKind::EmailTest),
        crate::COMMAND_EMAIL_TASK_READ => Some(NotificationResponseKind::EmailTaskRead),
        crate::COMMAND_EMAIL_TASK_WRITE => Some(NotificationResponseKind::EmailTaskWrite),
        crate::COMMAND_PUSH_INFO => Some(NotificationResponseKind::PushInfo),
        crate::COMMAND_PUSH_TASK_READ => Some(NotificationResponseKind::PushTask),
        crate::COMMAND_LED_READ => Some(NotificationResponseKind::LedRead),
        crate::COMMAND_LED_WRITE => Some(NotificationResponseKind::LedWrite),
        crate::COMMAND_BATTERY_LIST => Some(NotificationResponseKind::BatteryList),
        crate::COMMAND_BATTERY_INFO => Some(NotificationResponseKind::BatteryInfo),
        crate::COMMAND_FLOODLIGHT => Some(NotificationResponseKind::Floodlight),
        crate::COMMAND_FLOODLIGHT_TASK_READ => Some(NotificationResponseKind::FloodlightTaskRead),
        crate::COMMAND_FLOODLIGHT_TASK_WRITE => Some(NotificationResponseKind::FloodlightTaskWrite),
        crate::COMMAND_FLOODLIGHT_STATUS_LIST => {
            Some(NotificationResponseKind::FloodlightStatusList)
        }
        crate::COMMAND_SIREN_CONTROL => Some(NotificationResponseKind::SirenControl),
        crate::COMMAND_AUDIO_PLAY_INFO => Some(NotificationResponseKind::AudioPlayInfo),
        _ => None,
    }
}

pub fn build_request(
    cmd: &NotificationCommand,
    buf: &mut [u8],
) -> Result<(PacketHeader, usize), BcError> {
    match cmd {
        NotificationCommand::GetEmail { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("Email", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_EMAIL_READ, len), len))
        }
        NotificationCommand::SetEmail(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("Email", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.text_element("smtpServer", cfg.smtp_server.as_str());
                b.u32_element("smtpPort", cfg.smtp_port);
                b.text_element("sender", cfg.sender.as_str());
                b.text_element("ssl", if cfg.ssl { "1" } else { "0" });
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_EMAIL_WRITE, len), len))
        }
        NotificationCommand::TestEmail => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("EmailTest", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_EMAIL_TEST, len), len))
        }
        NotificationCommand::GetEmailTask { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("EmailTask", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_EMAIL_TASK_READ, len), len))
        }
        NotificationCommand::SetEmailTask(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("EmailTask", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_EMAIL_TASK_WRITE, len), len))
        }
        NotificationCommand::GetPushInfo { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PushInfo", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PUSH_INFO, len), len))
        }
        NotificationCommand::GetPushTask { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("PushTask", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_PUSH_TASK_READ, len), len))
        }
        NotificationCommand::GetLedState { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("LedState", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_LED_READ, len), len))
        }
        NotificationCommand::SetLedState(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("LedState", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.text_element("state", cfg.state.as_str());
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_LED_WRITE, len), len))
        }
        NotificationCommand::GetBatteryList => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("BatteryList", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_BATTERY_LIST, len), len))
        }
        NotificationCommand::GetBatteryInfo { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("BatteryInfo", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_BATTERY_INFO, len), len))
        }
        NotificationCommand::SetFloodlight(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("Floodlight", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_FLOODLIGHT, len), len))
        }
        NotificationCommand::GetFloodlightTask { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("FloodlightTask", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_FLOODLIGHT_TASK_READ, len), len))
        }
        NotificationCommand::SetFloodlightTask(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("FloodlightTask", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.u32_element("brightness", cfg.brightness);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_FLOODLIGHT_TASK_WRITE, len), len))
        }
        NotificationCommand::GetFloodlightStatusList => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("FloodlightStatusList", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_FLOODLIGHT_STATUS_LIST, len), len))
        }
        NotificationCommand::SirenControl { channel, enabled } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("SirenControl", "1.1");
                b.u8_element("channelId", *channel);
                b.text_element("enable", if *enabled { "1" } else { "0" });
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_SIREN_CONTROL, len), len))
        }
        NotificationCommand::GetAudioPlayInfo { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("AudioPlayInfo", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_AUDIO_PLAY_INFO, len), len))
        }
    }
}

pub fn parse_response(
    kind: NotificationResponseKind,
    body: &[u8],
) -> Result<NotificationEvent, BcError> {
    match kind {
        NotificationResponseKind::EmailRead => {
            let mut cfg = EmailConfig {
                channel: 0,
                enabled: false,
                smtp_server: ArrayString::new(),
                smtp_port: 0,
                sender: ArrayString::new(),
                ssl: false,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cfg.channel = v;
                    }
                }
                "enable" => cfg.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                "smtpServer" => {
                    let _ = ArrayString::try_from(text).map(|s| cfg.smtp_server = s);
                }
                "smtpPort" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cfg.smtp_port = v;
                    }
                }
                "sender" => {
                    let _ = ArrayString::try_from(text).map(|s| cfg.sender = s);
                }
                "ssl" => cfg.ssl = text == "1" || text.eq_ignore_ascii_case("true"),
                _ => {}
            })?;
            Ok(NotificationEvent::Email(cfg))
        }
        NotificationResponseKind::EmailWrite => Ok(NotificationEvent::EmailAck),
        NotificationResponseKind::EmailTest => {
            let mut success = false;
            xml::parse_xml(body, |name, text| {
                if name == "result" || name == "status" {
                    success = text == "1" || text == "ok" || text.eq_ignore_ascii_case("true");
                }
            })?;
            Ok(NotificationEvent::EmailTestResult { success })
        }
        NotificationResponseKind::EmailTaskRead => {
            let mut cfg = EmailTaskConfig {
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
            Ok(NotificationEvent::EmailTask(cfg))
        }
        NotificationResponseKind::EmailTaskWrite => Ok(NotificationEvent::EmailTaskAck),
        NotificationResponseKind::PushInfo => {
            let mut data = PushInfoData {
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
            Ok(NotificationEvent::PushInfo(data))
        }
        NotificationResponseKind::PushTask => {
            let mut data = PushTaskData {
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
            Ok(NotificationEvent::PushTask(data))
        }
        NotificationResponseKind::LedRead => {
            let mut cfg = LedStateConfig {
                channel: 0,
                enabled: false,
                state: ArrayString::new(),
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cfg.channel = v;
                    }
                }
                "enable" => cfg.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                "state" => {
                    let _ = ArrayString::try_from(text).map(|s| cfg.state = s);
                }
                _ => {}
            })?;
            Ok(NotificationEvent::LedState(cfg))
        }
        NotificationResponseKind::LedWrite => Ok(NotificationEvent::LedStateAck),
        NotificationResponseKind::BatteryList => {
            let entries = parse_battery_entries(body)?;
            Ok(NotificationEvent::BatteryList(entries))
        }
        NotificationResponseKind::BatteryInfo => {
            let mut data = BatteryInfoData {
                channel: 0,
                capacity: 0,
                temperature: 0,
                charging: false,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        data.channel = v;
                    }
                }
                "capacity" | "batteryPercent" => {
                    if let Ok(v) = text.parse::<u32>() {
                        data.capacity = v;
                    }
                }
                "temperature" => {
                    if let Ok(v) = text.parse::<i32>() {
                        data.temperature = v;
                    }
                }
                "charging" | "chargeStatus" => {
                    data.charging = text == "1" || text.eq_ignore_ascii_case("true");
                }
                _ => {}
            })?;
            Ok(NotificationEvent::BatteryInfo(data))
        }
        NotificationResponseKind::Floodlight => Ok(NotificationEvent::FloodlightAck),
        NotificationResponseKind::FloodlightTaskRead => {
            let mut cfg = FloodlightTaskConfig {
                channel: 0,
                enabled: false,
                brightness: 0,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cfg.channel = v;
                    }
                }
                "enable" => cfg.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                "brightness" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cfg.brightness = v;
                    }
                }
                _ => {}
            })?;
            Ok(NotificationEvent::FloodlightTask(cfg))
        }
        NotificationResponseKind::FloodlightTaskWrite => Ok(NotificationEvent::FloodlightTaskAck),
        NotificationResponseKind::FloodlightStatusList => {
            let entries = parse_floodlight_status_entries(body)?;
            Ok(NotificationEvent::FloodlightStatusList(entries))
        }
        NotificationResponseKind::SirenControl => Ok(NotificationEvent::SirenControlAck),
        NotificationResponseKind::AudioPlayInfo => {
            let mut data = AudioPlayInfoData {
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
            Ok(NotificationEvent::AudioPlayInfo(data))
        }
    }
}

fn parse_battery_entries(body: &[u8]) -> Result<Vec<BatteryInfoData>, BcError> {
    let mut entries = Vec::new();
    let mut current = BatteryInfoData {
        channel: 0,
        capacity: 0,
        temperature: 0,
        charging: false,
    };
    let mut seen_channel = false;

    xml::parse_xml(body, |name, text| match name {
        "channelId" => {
            if seen_channel {
                entries.push(current);
                current = BatteryInfoData {
                    channel: 0,
                    capacity: 0,
                    temperature: 0,
                    charging: false,
                };
            }
            if let Ok(v) = text.parse::<u8>() {
                current.channel = v;
            }
            seen_channel = true;
        }
        "capacity" | "batteryPercent" => {
            if let Ok(v) = text.parse::<u32>() {
                current.capacity = v;
            }
        }
        "temperature" => {
            if let Ok(v) = text.parse::<i32>() {
                current.temperature = v;
            }
        }
        "charging" | "chargeStatus" => {
            current.charging = text == "1" || text.eq_ignore_ascii_case("true");
        }
        _ => {}
    })?;

    if seen_channel {
        entries.push(current);
    }

    Ok(entries)
}

fn parse_floodlight_status_entries(body: &[u8]) -> Result<Vec<FloodlightStatusData>, BcError> {
    let mut entries = Vec::new();
    let mut current = FloodlightStatusData {
        channel: 0,
        enabled: false,
    };
    let mut seen_channel = false;

    xml::parse_xml(body, |name, text| match name {
        "channelId" => {
            if seen_channel {
                entries.push(current);
                current = FloodlightStatusData {
                    channel: 0,
                    enabled: false,
                };
            }
            if let Ok(v) = text.parse::<u8>() {
                current.channel = v;
            }
            seen_channel = true;
        }
        "enable" | "status" => current.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
        _ => {}
    })?;

    if seen_channel {
        entries.push(current);
    }

    Ok(entries)
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
    fn classify_notification_ids() {
        assert!(matches!(
            classify_response(crate::COMMAND_EMAIL_READ),
            Some(NotificationResponseKind::EmailRead)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_EMAIL_WRITE),
            Some(NotificationResponseKind::EmailWrite)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_EMAIL_TEST),
            Some(NotificationResponseKind::EmailTest)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_EMAIL_TASK_READ),
            Some(NotificationResponseKind::EmailTaskRead)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_EMAIL_TASK_WRITE),
            Some(NotificationResponseKind::EmailTaskWrite)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_PUSH_INFO),
            Some(NotificationResponseKind::PushInfo)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_PUSH_TASK_READ),
            Some(NotificationResponseKind::PushTask)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_LED_READ),
            Some(NotificationResponseKind::LedRead)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_LED_WRITE),
            Some(NotificationResponseKind::LedWrite)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_BATTERY_LIST),
            Some(NotificationResponseKind::BatteryList)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_BATTERY_INFO),
            Some(NotificationResponseKind::BatteryInfo)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_FLOODLIGHT),
            Some(NotificationResponseKind::Floodlight)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_FLOODLIGHT_TASK_READ),
            Some(NotificationResponseKind::FloodlightTaskRead)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_FLOODLIGHT_TASK_WRITE),
            Some(NotificationResponseKind::FloodlightTaskWrite)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_FLOODLIGHT_STATUS_LIST),
            Some(NotificationResponseKind::FloodlightStatusList)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_SIREN_CONTROL),
            Some(NotificationResponseKind::SirenControl)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_AUDIO_PLAY_INFO),
            Some(NotificationResponseKind::AudioPlayInfo)
        ));
        assert!(classify_response(999).is_none());
    }

    #[test]
    fn build_get_email() {
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&NotificationCommand::GetEmail { channel: 0 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_EMAIL_READ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<Email"));
    }

    #[test]
    fn build_set_email() {
        let cfg = EmailConfig {
            channel: 0,
            enabled: true,
            smtp_server: ArrayString::try_from("smtp.example.com").unwrap(),
            smtp_port: 587,
            sender: ArrayString::try_from("cam@example.com").unwrap(),
            ssl: true,
        };
        let mut buf = [0u8; 1024];
        let (hdr, len) = build_request(&NotificationCommand::SetEmail(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_EMAIL_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<smtpServer>smtp.example.com</smtpServer>"));
        assert!(xml.contains("<smtpPort>587</smtpPort>"));
        assert!(xml.contains("<sender>cam@example.com</sender>"));
        assert!(xml.contains("<ssl>1</ssl>"));
    }

    #[test]
    fn build_test_email() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&NotificationCommand::TestEmail, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_EMAIL_TEST);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<EmailTest"));
    }

    #[test]
    fn build_set_email_task() {
        let cfg = EmailTaskConfig {
            channel: 0,
            enabled: true,
        };
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&NotificationCommand::SetEmailTask(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_EMAIL_TASK_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<enable>1</enable>"));
    }

    #[test]
    fn build_get_led_state() {
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&NotificationCommand::GetLedState { channel: 0 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_LED_READ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<LedState"));
    }

    #[test]
    fn build_set_led_state() {
        let cfg = LedStateConfig {
            channel: 0,
            enabled: true,
            state: ArrayString::try_from("auto").unwrap(),
        };
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&NotificationCommand::SetLedState(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_LED_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<enable>1</enable>"));
        assert!(xml.contains("<state>auto</state>"));
    }

    #[test]
    fn build_get_battery_list() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&NotificationCommand::GetBatteryList, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_BATTERY_LIST);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<BatteryList"));
    }

    #[test]
    fn build_set_floodlight() {
        let cfg = ManualLightState {
            channel: 0,
            enabled: true,
        };
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&NotificationCommand::SetFloodlight(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_FLOODLIGHT);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<enable>1</enable>"));
    }

    #[test]
    fn build_set_floodlight_task() {
        let cfg = FloodlightTaskConfig {
            channel: 0,
            enabled: true,
            brightness: 80,
        };
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&NotificationCommand::SetFloodlightTask(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_FLOODLIGHT_TASK_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<brightness>80</brightness>"));
    }

    #[test]
    fn build_siren_control() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &NotificationCommand::SirenControl {
                channel: 0,
                enabled: true,
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_SIREN_CONTROL);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<enable>1</enable>"));
    }

    #[test]
    fn build_get_audio_play_info() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &NotificationCommand::GetAudioPlayInfo { channel: 0 },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_AUDIO_PLAY_INFO);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<AudioPlayInfo"));
    }

    #[test]
    fn parse_email_response() {
        let xml = b"<body>\
            <Email version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
                <smtpServer>smtp.example.com</smtpServer>\
                <smtpPort>587</smtpPort>\
                <sender>cam@example.com</sender>\
                <ssl>1</ssl>\
            </Email>\
        </body>";
        let event = parse_response(NotificationResponseKind::EmailRead, xml).unwrap();
        match event {
            NotificationEvent::Email(cfg) => {
                assert!(cfg.enabled);
                assert_eq!(cfg.smtp_server.as_str(), "smtp.example.com");
                assert_eq!(cfg.smtp_port, 587);
                assert_eq!(cfg.sender.as_str(), "cam@example.com");
                assert!(cfg.ssl);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_email_ack() {
        let xml = b"<body><Email version=\"1.1\"></Email></body>";
        let event = parse_response(NotificationResponseKind::EmailWrite, xml).unwrap();
        assert!(matches!(event, NotificationEvent::EmailAck));
    }

    #[test]
    fn parse_email_test_result() {
        let xml = b"<body>\
            <EmailTest version=\"1.1\">\
                <result>ok</result>\
            </EmailTest>\
        </body>";
        let event = parse_response(NotificationResponseKind::EmailTest, xml).unwrap();
        match event {
            NotificationEvent::EmailTestResult { success } => assert!(success),
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_email_task() {
        let xml = b"<body>\
            <EmailTask version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
            </EmailTask>\
        </body>";
        let event = parse_response(NotificationResponseKind::EmailTaskRead, xml).unwrap();
        match event {
            NotificationEvent::EmailTask(cfg) => {
                assert!(cfg.enabled);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_push_info() {
        let xml = b"<body>\
            <PushInfo version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
            </PushInfo>\
        </body>";
        let event = parse_response(NotificationResponseKind::PushInfo, xml).unwrap();
        match event {
            NotificationEvent::PushInfo(data) => {
                assert!(data.enabled);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_push_task() {
        let xml = b"<body>\
            <PushTask version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>0</enable>\
            </PushTask>\
        </body>";
        let event = parse_response(NotificationResponseKind::PushTask, xml).unwrap();
        match event {
            NotificationEvent::PushTask(data) => {
                assert!(!data.enabled);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_led_state() {
        let xml = b"<body>\
            <LedState version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
                <state>auto</state>\
            </LedState>\
        </body>";
        let event = parse_response(NotificationResponseKind::LedRead, xml).unwrap();
        match event {
            NotificationEvent::LedState(cfg) => {
                assert!(cfg.enabled);
                assert_eq!(cfg.state.as_str(), "auto");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_led_state_ack() {
        let xml = b"<body><LedState version=\"1.1\"></LedState></body>";
        let event = parse_response(NotificationResponseKind::LedWrite, xml).unwrap();
        assert!(matches!(event, NotificationEvent::LedStateAck));
    }

    #[test]
    fn parse_battery_info() {
        let xml = b"<body>\
            <BatteryInfo version=\"1.1\">\
                <channelId>0</channelId>\
                <capacity>85</capacity>\
                <temperature>25</temperature>\
                <charging>1</charging>\
            </BatteryInfo>\
        </body>";
        let event = parse_response(NotificationResponseKind::BatteryInfo, xml).unwrap();
        match event {
            NotificationEvent::BatteryInfo(data) => {
                assert_eq!(data.capacity, 85);
                assert_eq!(data.temperature, 25);
                assert!(data.charging);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_battery_list() {
        let xml = b"<body>\
            <BatteryList version=\"1.1\">\
                <channelId>0</channelId>\
                <capacity>85</capacity>\
                <temperature>25</temperature>\
                <charging>1</charging>\
                <channelId>1</channelId>\
                <capacity>42</capacity>\
                <temperature>30</temperature>\
                <charging>0</charging>\
            </BatteryList>\
        </body>";
        let event = parse_response(NotificationResponseKind::BatteryList, xml).unwrap();
        match event {
            NotificationEvent::BatteryList(list) => {
                assert_eq!(list.len(), 2);
                assert_eq!(list[0].capacity, 85);
                assert!(list[0].charging);
                assert_eq!(list[1].capacity, 42);
                assert!(!list[1].charging);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_floodlight_ack() {
        let xml = b"<body><Floodlight version=\"1.1\"></Floodlight></body>";
        let event = parse_response(NotificationResponseKind::Floodlight, xml).unwrap();
        assert!(matches!(event, NotificationEvent::FloodlightAck));
    }

    #[test]
    fn parse_floodlight_task() {
        let xml = b"<body>\
            <FloodlightTask version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
                <brightness>80</brightness>\
            </FloodlightTask>\
        </body>";
        let event = parse_response(NotificationResponseKind::FloodlightTaskRead, xml).unwrap();
        match event {
            NotificationEvent::FloodlightTask(cfg) => {
                assert!(cfg.enabled);
                assert_eq!(cfg.brightness, 80);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_floodlight_task_ack() {
        let xml = b"<body><FloodlightTask version=\"1.1\"></FloodlightTask></body>";
        let event = parse_response(NotificationResponseKind::FloodlightTaskWrite, xml).unwrap();
        assert!(matches!(event, NotificationEvent::FloodlightTaskAck));
    }

    #[test]
    fn parse_floodlight_status_list() {
        let xml = b"<body>\
            <FloodlightStatusList version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
                <channelId>1</channelId>\
                <enable>0</enable>\
            </FloodlightStatusList>\
        </body>";
        let event = parse_response(NotificationResponseKind::FloodlightStatusList, xml).unwrap();
        match event {
            NotificationEvent::FloodlightStatusList(list) => {
                assert_eq!(list.len(), 2);
                assert!(list[0].enabled);
                assert!(!list[1].enabled);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_siren_control_ack() {
        let xml = b"<body><SirenControl version=\"1.1\"></SirenControl></body>";
        let event = parse_response(NotificationResponseKind::SirenControl, xml).unwrap();
        assert!(matches!(event, NotificationEvent::SirenControlAck));
    }

    #[test]
    fn parse_audio_play_info() {
        let xml = b"<body>\
            <AudioPlayInfo version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
            </AudioPlayInfo>\
        </body>";
        let event = parse_response(NotificationResponseKind::AudioPlayInfo, xml).unwrap();
        match event {
            NotificationEvent::AudioPlayInfo(data) => {
                assert!(data.enabled);
            }
            _ => panic!("wrong event"),
        }
    }
}
