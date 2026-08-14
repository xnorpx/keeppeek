//! Device & system query/config commands.
//!
//! Provides builders and parsers for firmware details, capability support,
//! capability details, system settings, time configuration, reboot, account
//! directories, and firmware file information.

use crate::{error::BcError, header::PacketHeader, magic::*, xml};
use arrayvec::ArrayString;

const NAME_CAP: usize = 64;
const DETAIL_CAP: usize = 128;

/// Device command (client → camera).
#[derive(Debug, Clone, Copy)]
pub enum DeviceCommand {
    /// Query firmware version and device info (ID 80).
    GetFirmwareDetails,
    /// Query ability support flags (ID 58).
    GetAbilitySupport,
    /// Query detailed ability info for a channel (ID 151).
    GetCapabilityDetails { channel: u8 },
    /// Query system general (time, timezone, language) (ID 104).
    GetSystemSettings,
    /// Set system clock (ID 287).
    SetTimeCfg(TimeCfg),
    /// Reboot the camera (ID 23).
    Reboot,
    /// Query user list (ID 59).
    GetAccountDirectory,
    /// Query config/firmware file info (ID 67).
    GetConfigFileInfo,
}

/// Device event (camera → client).
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum DeviceEvent {
    FirmwareDetails(FirmwareDetails),
    AbilitySupport(AbilitySupport),
    CapabilityDetails(CapabilityDetails),
    SystemSettings(SystemSettings),
    TimeCfgAck,
    RebootAck,
    AccountDirectory(AccountDirectory),
    ConfigFileInfo(ConfigFileInfo),
}

/// Version / device info parsed from response to ID 80.
#[derive(Debug, Clone)]
pub struct FirmwareDetails {
    pub firmware_version: ArrayString<NAME_CAP>,
    pub hardware_version: ArrayString<NAME_CAP>,
    pub device_name: ArrayString<NAME_CAP>,
    pub serial: ArrayString<{ crate::SERIAL_CAP }>,
    pub build_day: ArrayString<NAME_CAP>,
    pub config_version: ArrayString<NAME_CAP>,
    pub detail: ArrayString<DETAIL_CAP>,
}

/// Ability support flags from response to ID 58.
#[derive(Debug, Clone, Copy)]
pub struct AbilitySupport {
    pub support_ptz: bool,
    pub support_talk: bool,
    pub support_record: bool,
    pub support_alarm: bool,
    pub support_wifi: bool,
    pub support_cloud: bool,
}

/// Ability info from response to ID 151.
#[derive(Debug, Clone, Copy)]
pub struct CapabilityDetails {
    pub channel: u8,
    pub main_stream_supported: bool,
    pub sub_stream_supported: bool,
    pub audio_supported: bool,
    pub ptz_supported: bool,
}

/// System general settings from response to ID 104.
#[derive(Debug, Clone)]
pub struct SystemSettings {
    pub timezone: i32,
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub device_name: ArrayString<NAME_CAP>,
    pub language: ArrayString<NAME_CAP>,
}

/// Time config for SetTimeCfg (ID 287).
#[derive(Debug, Clone, Copy)]
pub struct TimeCfg {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// User list from response to ID 59.
#[derive(Debug, Clone)]
pub struct AccountDirectory {
    pub user_count: u32,
}

/// Config file info from response to ID 67.
#[derive(Debug, Clone)]
pub struct ConfigFileInfo {
    pub firmware_version: ArrayString<NAME_CAP>,
}

/// Internal response discriminant (stored in EventKind, parsed lazily).
#[derive(Debug, Clone, Copy)]
pub enum DeviceResponseKind {
    FirmwareDetails,
    AbilitySupport,
    CapabilityDetails,
    SystemSettings,
    TimeCfgAck,
    RebootAck,
    AccountDirectory,
    ConfigFileInfo,
}

/// Classify an incoming msg_id as a device response.
pub const fn classify_response(msg_id: u32) -> Option<DeviceResponseKind> {
    match msg_id {
        crate::COMMAND_FIRMWARE_DETAILS => Some(DeviceResponseKind::FirmwareDetails),
        crate::COMMAND_ABILITY_SUPPORT => Some(DeviceResponseKind::AbilitySupport),
        crate::COMMAND_CAPABILITY_DETAILS => Some(DeviceResponseKind::CapabilityDetails),
        crate::COMMAND_SYSTEM_SETTINGS => Some(DeviceResponseKind::SystemSettings),
        crate::COMMAND_TIME_CFG => Some(DeviceResponseKind::TimeCfgAck),
        crate::COMMAND_REBOOT => Some(DeviceResponseKind::RebootAck),
        crate::COMMAND_ACCOUNT_DIRECTORY => Some(DeviceResponseKind::AccountDirectory),
        crate::COMMAND_CONFIG_FILE_INFO => Some(DeviceResponseKind::ConfigFileInfo),
        _ => None,
    }
}

/// Build the wire header + XML body for a device command.
/// Returns (header, xml_bytes_written).
pub fn build_request(
    cmd: &DeviceCommand,
    buf: &mut [u8],
) -> Result<(PacketHeader, usize), BcError> {
    match cmd {
        DeviceCommand::GetFirmwareDetails => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("VersionInfo", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_FIRMWARE_DETAILS, len), len))
        }
        DeviceCommand::GetAbilitySupport => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("AbilitySupport", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_ABILITY_SUPPORT, len), len))
        }
        DeviceCommand::GetCapabilityDetails { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("AbilityInfo", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_CAPABILITY_DETAILS, len), len))
        }
        DeviceCommand::GetSystemSettings => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("SystemGeneral", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_SYSTEM_SETTINGS, len), len))
        }
        DeviceCommand::SetTimeCfg(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("TimeCfg", "1.1");
                b.u32_element("year", cfg.year);
                b.u32_element("month", cfg.month);
                b.u32_element("day", cfg.day);
                b.u32_element("hour", cfg.hour);
                b.u32_element("minute", cfg.minute);
                b.u32_element("second", cfg.second);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_TIME_CFG, len), len))
        }
        DeviceCommand::Reboot => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("Reboot", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_REBOOT, len), len))
        }
        DeviceCommand::GetAccountDirectory => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("UserList", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_ACCOUNT_DIRECTORY, len), len))
        }
        DeviceCommand::GetConfigFileInfo => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("ConfigFileInfo", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_CONFIG_FILE_INFO, len), len))
        }
    }
}

/// Parse a device response body into a DeviceEvent.
pub fn parse_response(kind: DeviceResponseKind, body: &[u8]) -> Result<DeviceEvent, BcError> {
    match kind {
        DeviceResponseKind::FirmwareDetails => {
            let mut info = FirmwareDetails {
                firmware_version: ArrayString::new(),
                hardware_version: ArrayString::new(),
                device_name: ArrayString::new(),
                serial: ArrayString::new(),
                build_day: ArrayString::new(),
                config_version: ArrayString::new(),
                detail: ArrayString::new(),
            };
            xml::parse_xml(body, |name, text| match name {
                "firmVer" | "firmwareVersion" => {
                    let _ = ArrayString::try_from(text).map(|s| info.firmware_version = s);
                }
                "hardVer" | "hardwareVersion" => {
                    let _ = ArrayString::try_from(text).map(|s| info.hardware_version = s);
                }
                "name" | "deviceName" => {
                    let _ = ArrayString::try_from(text).map(|s| info.device_name = s);
                }
                "serial" | "serialNumber" => {
                    let _ = ArrayString::try_from(text).map(|s| info.serial = s);
                }
                "buildDay" | "buildDate" => {
                    let _ = ArrayString::try_from(text).map(|s| info.build_day = s);
                }
                "cfgVer" | "configVersion" => {
                    let _ = ArrayString::try_from(text).map(|s| info.config_version = s);
                }
                "detail" => {
                    let _ = ArrayString::try_from(text).map(|s| info.detail = s);
                }
                _ => {}
            })?;
            Ok(DeviceEvent::FirmwareDetails(info))
        }
        DeviceResponseKind::AbilitySupport => {
            let mut ability = AbilitySupport {
                support_ptz: false,
                support_talk: false,
                support_record: false,
                support_alarm: false,
                support_wifi: false,
                support_cloud: false,
            };
            xml::parse_xml(body, |name, text| {
                let flag = text == "1" || text.eq_ignore_ascii_case("true");
                match name {
                    "supportPtz" | "ptz" => ability.support_ptz = flag,
                    "supportTalk" | "talk" => ability.support_talk = flag,
                    "supportRecord" | "record" => ability.support_record = flag,
                    "supportAlarm" | "alarm" => ability.support_alarm = flag,
                    "supportWifi" | "wifi" => ability.support_wifi = flag,
                    "supportCloud" | "cloud" => ability.support_cloud = flag,
                    _ => {}
                }
            })?;
            Ok(DeviceEvent::AbilitySupport(ability))
        }
        DeviceResponseKind::CapabilityDetails => {
            let mut info = CapabilityDetails {
                channel: 0,
                main_stream_supported: false,
                sub_stream_supported: false,
                audio_supported: false,
                ptz_supported: false,
            };
            xml::parse_xml(body, |name, text| {
                let flag = text == "1" || text.eq_ignore_ascii_case("true");
                match name {
                    "channelId" => {
                        if let Ok(v) = text.parse::<u8>() {
                            info.channel = v;
                        }
                    }
                    "mainStream" => info.main_stream_supported = flag,
                    "subStream" => info.sub_stream_supported = flag,
                    "audio" => info.audio_supported = flag,
                    "ptz" => info.ptz_supported = flag,
                    _ => {}
                }
            })?;
            Ok(DeviceEvent::CapabilityDetails(info))
        }
        DeviceResponseKind::SystemSettings => {
            let mut sg = SystemSettings {
                timezone: 0,
                year: 0,
                month: 0,
                day: 0,
                hour: 0,
                minute: 0,
                second: 0,
                device_name: ArrayString::new(),
                language: ArrayString::new(),
            };
            xml::parse_xml(body, |name, text| match name {
                "timeZone" | "timezone" => {
                    if let Ok(v) = text.parse::<i32>() {
                        sg.timezone = v;
                    }
                }
                "year" => {
                    if let Ok(v) = text.parse::<u32>() {
                        sg.year = v;
                    }
                }
                "month" => {
                    if let Ok(v) = text.parse::<u32>() {
                        sg.month = v;
                    }
                }
                "day" => {
                    if let Ok(v) = text.parse::<u32>() {
                        sg.day = v;
                    }
                }
                "hour" => {
                    if let Ok(v) = text.parse::<u32>() {
                        sg.hour = v;
                    }
                }
                "minute" => {
                    if let Ok(v) = text.parse::<u32>() {
                        sg.minute = v;
                    }
                }
                "second" => {
                    if let Ok(v) = text.parse::<u32>() {
                        sg.second = v;
                    }
                }
                "deviceName" | "name" => {
                    let _ = ArrayString::try_from(text).map(|s| sg.device_name = s);
                }
                "language" => {
                    let _ = ArrayString::try_from(text).map(|s| sg.language = s);
                }
                _ => {}
            })?;
            Ok(DeviceEvent::SystemSettings(sg))
        }
        DeviceResponseKind::TimeCfgAck => Ok(DeviceEvent::TimeCfgAck),
        DeviceResponseKind::RebootAck => Ok(DeviceEvent::RebootAck),
        DeviceResponseKind::AccountDirectory => {
            let mut list = AccountDirectory { user_count: 0 };
            xml::parse_xml(body, |name, text| {
                if (name == "userCount" || name == "count")
                    && let Ok(v) = text.parse::<u32>()
                {
                    list.user_count = v;
                }
            })?;
            Ok(DeviceEvent::AccountDirectory(list))
        }
        DeviceResponseKind::ConfigFileInfo => {
            let mut info = ConfigFileInfo {
                firmware_version: ArrayString::new(),
            };
            xml::parse_xml(body, |name, text| {
                if name == "firmVer" || name == "firmwareVersion" {
                    let _ = ArrayString::try_from(text).map(|s| info.firmware_version = s);
                }
            })?;
            Ok(DeviceEvent::ConfigFileInfo(info))
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
    fn classify_known_ids() {
        assert!(matches!(
            classify_response(crate::COMMAND_FIRMWARE_DETAILS),
            Some(DeviceResponseKind::FirmwareDetails)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_REBOOT),
            Some(DeviceResponseKind::RebootAck)
        ));
        assert!(classify_response(12345).is_none());
    }

    #[test]
    fn build_firmware_details_request() {
        let mut buf = [0u8; 512];
        let (header, len) = build_request(&DeviceCommand::GetFirmwareDetails, &mut buf).unwrap();
        assert_eq!(header.msg_id, crate::COMMAND_FIRMWARE_DETAILS);
        assert!(header.is_modern());
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<VersionInfo"));
        assert!(xml.contains("version=\"1.1\""));
    }

    #[test]
    fn build_capability_details_request() {
        let mut buf = [0u8; 512];
        let (header, len) = build_request(
            &DeviceCommand::GetCapabilityDetails { channel: 2 },
            &mut buf,
        )
        .unwrap();
        assert_eq!(header.msg_id, crate::COMMAND_CAPABILITY_DETAILS);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<channelId>2</channelId>"));
    }

    #[test]
    fn build_set_time_cfg() {
        let cfg = TimeCfg {
            year: 2025,
            month: 12,
            day: 25,
            hour: 10,
            minute: 30,
            second: 0,
        };
        let mut buf = [0u8; 1024];
        let (header, len) = build_request(&DeviceCommand::SetTimeCfg(cfg), &mut buf).unwrap();
        assert_eq!(header.msg_id, crate::COMMAND_TIME_CFG);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<year>2025</year>"));
        assert!(xml.contains("<month>12</month>"));
    }

    #[test]
    fn build_reboot_request() {
        let mut buf = [0u8; 512];
        let (header, len) = build_request(&DeviceCommand::Reboot, &mut buf).unwrap();
        assert_eq!(header.msg_id, crate::COMMAND_REBOOT);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<Reboot"));
    }

    #[test]
    fn parse_firmware_details_response() {
        let xml = b"<body>\
            <VersionInfo version=\"1.1\">\
                <firmVer>v3.1.0</firmVer>\
                <hardVer>IPC_1234</hardVer>\
                <name>MyCamera</name>\
                <serial>ABC123</serial>\
                <buildDay>2025-01-15</buildDay>\
                <cfgVer>v1.0</cfgVer>\
                <detail>some detail</detail>\
            </VersionInfo>\
        </body>";
        let event = parse_response(DeviceResponseKind::FirmwareDetails, xml).unwrap();
        match event {
            DeviceEvent::FirmwareDetails(info) => {
                assert_eq!(info.firmware_version.as_str(), "v3.1.0");
                assert_eq!(info.hardware_version.as_str(), "IPC_1234");
                assert_eq!(info.device_name.as_str(), "MyCamera");
                assert_eq!(info.serial.as_str(), "ABC123");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_ability_support_response() {
        let xml = b"<body>\
            <AbilitySupport version=\"1.1\">\
                <supportPtz>1</supportPtz>\
                <supportTalk>0</supportTalk>\
                <supportRecord>1</supportRecord>\
                <supportAlarm>1</supportAlarm>\
                <supportWifi>0</supportWifi>\
                <supportCloud>1</supportCloud>\
            </AbilitySupport>\
        </body>";
        let event = parse_response(DeviceResponseKind::AbilitySupport, xml).unwrap();
        match event {
            DeviceEvent::AbilitySupport(a) => {
                assert!(a.support_ptz);
                assert!(!a.support_talk);
                assert!(a.support_record);
                assert!(a.support_alarm);
                assert!(!a.support_wifi);
                assert!(a.support_cloud);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_system_settings_response() {
        let xml = b"<body>\
            <SystemGeneral version=\"1.1\">\
                <timeZone>28800</timeZone>\
                <year>2025</year>\
                <month>2</month>\
                <day>15</day>\
                <hour>10</hour>\
                <minute>30</minute>\
                <second>0</second>\
                <deviceName>Backyard</deviceName>\
                <language>English</language>\
            </SystemGeneral>\
        </body>";
        let event = parse_response(DeviceResponseKind::SystemSettings, xml).unwrap();
        match event {
            DeviceEvent::SystemSettings(sg) => {
                assert_eq!(sg.timezone, 28800);
                assert_eq!(sg.year, 2025);
                assert_eq!(sg.device_name.as_str(), "Backyard");
                assert_eq!(sg.language.as_str(), "English");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_reboot_ack() {
        let xml = b"<body><Reboot version=\"1.1\"></Reboot></body>";
        let event = parse_response(DeviceResponseKind::RebootAck, xml).unwrap();
        assert!(matches!(event, DeviceEvent::RebootAck));
    }

    #[test]
    fn unknown_xml_fields_tolerated() {
        let xml = b"<body>\
            <VersionInfo version=\"1.1\">\
                <firmVer>v1.0</firmVer>\
                <unknownField>whatever</unknownField>\
                <anotherOne>123</anotherOne>\
            </VersionInfo>\
        </body>";
        let event = parse_response(DeviceResponseKind::FirmwareDetails, xml).unwrap();
        match event {
            DeviceEvent::FirmwareDetails(info) => {
                assert_eq!(info.firmware_version.as_str(), "v1.0");
            }
            _ => panic!("wrong event"),
        }
    }
}
