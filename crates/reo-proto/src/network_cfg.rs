//! Network query/config commands.
//!
//! Provides builders and parsers for IP config, LinkType, WiFi signal/list,
//! Cellular info, and Cloud bind/key queries.

use crate::{error::BcError, header::PacketHeader, magic::*, xml};
use arrayvec::ArrayString;

const NAME_CAP: usize = 64;
const ADDR_CAP: usize = 64;

/// Network command (client → camera).
#[derive(Debug, Clone, Copy)]
#[allow(clippy::large_enum_variant)]
pub enum NetworkCommand {
    /// Read IP configuration (ID 76).
    GetIp,
    /// Write IP configuration (ID 77).
    SetIp(IpConfig),
    /// Query link type — ethernet or wifi (ID 93).
    GetLinkType,
    /// Query WiFi signal strength (ID 115).
    GetWifiSignal,
    /// Query available WiFi networks (ID 116).
    GetWifiList,
    /// Query cellular (3G/4G) info (ID 255).
    GetCellularInfo,
    /// Query cloud bind info (ID 268).
    GetCloudBindInfo,
    /// Query cloud login key (ID 282).
    GetCloudLoginKey,
}

/// Network event (camera → client).
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum NetworkEvent {
    Ip(IpConfig),
    IpAck,
    LinkType(LinkTypeInfo),
    WifiSignal(WifiSignalInfo),
    WifiList(WifiListInfo),
    CellularInfo(CellularInfoData),
    CloudBindInfo(CloudBindInfoData),
    CloudLoginKey(CloudLoginKeyData),
}

/// IP configuration.
#[derive(Debug, Clone, Copy)]
pub struct IpConfig {
    pub dhcp: bool,
    pub ip: ArrayString<ADDR_CAP>,
    pub mask: ArrayString<ADDR_CAP>,
    pub gateway: ArrayString<ADDR_CAP>,
    pub dns1: ArrayString<ADDR_CAP>,
    pub dns2: ArrayString<ADDR_CAP>,
}

/// Link type information.
#[derive(Debug, Clone, Copy)]
pub struct LinkTypeInfo {
    pub link_type: ArrayString<NAME_CAP>,
}

/// WiFi signal strength.
#[derive(Debug, Clone, Copy)]
pub struct WifiSignalInfo {
    pub signal: u32,
}

/// WiFi network list entry count.
#[derive(Debug, Clone, Copy)]
pub struct WifiListInfo {
    pub count: u32,
}

/// Cellular (3G/4G) info.
#[derive(Debug, Clone, Copy)]
pub struct CellularInfoData {
    pub signal: u32,
    pub network_type: ArrayString<NAME_CAP>,
}

/// Cloud bind info.
#[derive(Debug, Clone, Copy)]
pub struct CloudBindInfoData {
    pub bound: bool,
}

/// Cloud login key.
#[derive(Debug, Clone, Copy)]
pub struct CloudLoginKeyData {
    pub key: ArrayString<NAME_CAP>,
}

#[derive(Debug, Clone, Copy)]
pub enum NetworkResponseKind {
    Ip,
    IpAck,
    LinkType,
    WifiSignal,
    WifiList,
    CellularInfo,
    CloudBindInfo,
    CloudLoginKey,
}

/// Classify an incoming msg_id as a network response.
pub const fn classify_response(msg_id: u32) -> Option<NetworkResponseKind> {
    match msg_id {
        crate::COMMAND_IP_READ => Some(NetworkResponseKind::Ip),
        crate::COMMAND_IP_WRITE => Some(NetworkResponseKind::IpAck),
        crate::COMMAND_LINK_TYPE => Some(NetworkResponseKind::LinkType),
        crate::COMMAND_WIFI_SIGNAL => Some(NetworkResponseKind::WifiSignal),
        crate::COMMAND_WIFI_LIST => Some(NetworkResponseKind::WifiList),
        crate::COMMAND_CELLULAR_INFO => Some(NetworkResponseKind::CellularInfo),
        crate::COMMAND_CLOUD_BIND_INFO => Some(NetworkResponseKind::CloudBindInfo),
        crate::COMMAND_CLOUD_LOGIN_KEY => Some(NetworkResponseKind::CloudLoginKey),
        _ => None,
    }
}

pub fn build_request(
    cmd: &NetworkCommand,
    buf: &mut [u8],
) -> Result<(PacketHeader, usize), BcError> {
    match cmd {
        NetworkCommand::GetIp => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("NetworkGeneral", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_IP_READ, len), len))
        }
        NetworkCommand::SetIp(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("NetworkGeneral", "1.1");
                b.text_element("dhcp", if cfg.dhcp { "1" } else { "0" });
                b.text_element("ip", cfg.ip.as_str());
                b.text_element("mask", cfg.mask.as_str());
                b.text_element("gateway", cfg.gateway.as_str());
                b.text_element("dns1", cfg.dns1.as_str());
                b.text_element("dns2", cfg.dns2.as_str());
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_IP_WRITE, len), len))
        }
        NetworkCommand::GetLinkType => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("LinkType", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_LINK_TYPE, len), len))
        }
        NetworkCommand::GetWifiSignal => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("WifiSignal", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_WIFI_SIGNAL, len), len))
        }
        NetworkCommand::GetWifiList => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("WifiList", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_WIFI_LIST, len), len))
        }
        NetworkCommand::GetCellularInfo => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("CellularInfo", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_CELLULAR_INFO, len), len))
        }
        NetworkCommand::GetCloudBindInfo => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("CloudBindInfo", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_CLOUD_BIND_INFO, len), len))
        }
        NetworkCommand::GetCloudLoginKey => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("CloudLoginKey", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_CLOUD_LOGIN_KEY, len), len))
        }
    }
}

pub fn parse_response(kind: NetworkResponseKind, body: &[u8]) -> Result<NetworkEvent, BcError> {
    match kind {
        NetworkResponseKind::Ip => {
            let mut cfg = IpConfig {
                dhcp: false,
                ip: ArrayString::new(),
                mask: ArrayString::new(),
                gateway: ArrayString::new(),
                dns1: ArrayString::new(),
                dns2: ArrayString::new(),
            };
            xml::parse_xml(body, |name, text| match name {
                "dhcp" => cfg.dhcp = text == "1" || text.eq_ignore_ascii_case("true"),
                "ip" | "ipAddr" => {
                    let _ = ArrayString::try_from(text).map(|s| cfg.ip = s);
                }
                "mask" | "netmask" | "subnetMask" => {
                    let _ = ArrayString::try_from(text).map(|s| cfg.mask = s);
                }
                "gateway" | "defaultGateway" => {
                    let _ = ArrayString::try_from(text).map(|s| cfg.gateway = s);
                }
                "dns1" | "primaryDns" => {
                    let _ = ArrayString::try_from(text).map(|s| cfg.dns1 = s);
                }
                "dns2" | "secondaryDns" => {
                    let _ = ArrayString::try_from(text).map(|s| cfg.dns2 = s);
                }
                _ => {}
            })?;
            Ok(NetworkEvent::Ip(cfg))
        }
        NetworkResponseKind::IpAck => Ok(NetworkEvent::IpAck),
        NetworkResponseKind::LinkType => {
            let mut info = LinkTypeInfo {
                link_type: ArrayString::new(),
            };
            xml::parse_xml(body, |name, text| {
                if name == "type" || name == "linkType" {
                    let _ = ArrayString::try_from(text).map(|s| info.link_type = s);
                }
            })?;
            Ok(NetworkEvent::LinkType(info))
        }
        NetworkResponseKind::WifiSignal => {
            let mut info = WifiSignalInfo { signal: 0 };
            xml::parse_xml(body, |name, text| {
                if (name == "signal" || name == "signalStrength")
                    && let Ok(v) = text.parse::<u32>()
                {
                    info.signal = v;
                }
            })?;
            Ok(NetworkEvent::WifiSignal(info))
        }
        NetworkResponseKind::WifiList => {
            let mut info = WifiListInfo { count: 0 };
            xml::parse_xml(body, |name, text| {
                if (name == "count" || name == "wifiCount")
                    && let Ok(v) = text.parse::<u32>()
                {
                    info.count = v;
                }
            })?;
            Ok(NetworkEvent::WifiList(info))
        }
        NetworkResponseKind::CellularInfo => {
            let mut info = CellularInfoData {
                signal: 0,
                network_type: ArrayString::new(),
            };
            xml::parse_xml(body, |name, text| match name {
                "signal" | "signalStrength" => {
                    if let Ok(v) = text.parse::<u32>() {
                        info.signal = v;
                    }
                }
                "networkType" | "type" => {
                    let _ = ArrayString::try_from(text).map(|s| info.network_type = s);
                }
                _ => {}
            })?;
            Ok(NetworkEvent::CellularInfo(info))
        }
        NetworkResponseKind::CloudBindInfo => {
            let mut info = CloudBindInfoData { bound: false };
            xml::parse_xml(body, |name, text| {
                if name == "bound" || name == "bindStatus" {
                    info.bound = text == "1" || text.eq_ignore_ascii_case("true");
                }
            })?;
            Ok(NetworkEvent::CloudBindInfo(info))
        }
        NetworkResponseKind::CloudLoginKey => {
            let mut info = CloudLoginKeyData {
                key: ArrayString::new(),
            };
            xml::parse_xml(body, |name, text| {
                if name == "key" || name == "loginKey" {
                    let _ = ArrayString::try_from(text).map(|s| info.key = s);
                }
            })?;
            Ok(NetworkEvent::CloudLoginKey(info))
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
    fn classify_network_ids() {
        assert!(matches!(
            classify_response(crate::COMMAND_IP_READ),
            Some(NetworkResponseKind::Ip)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_LINK_TYPE),
            Some(NetworkResponseKind::LinkType)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_WIFI_SIGNAL),
            Some(NetworkResponseKind::WifiSignal)
        ));
        assert!(classify_response(999).is_none());
    }

    #[test]
    fn build_get_ip() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&NetworkCommand::GetIp, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_IP_READ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<NetworkGeneral"));
    }

    #[test]
    fn build_set_ip() {
        let cfg = IpConfig {
            dhcp: false,
            ip: ArrayString::try_from("192.168.1.100").unwrap(),
            mask: ArrayString::try_from("255.255.255.0").unwrap(),
            gateway: ArrayString::try_from("192.168.1.1").unwrap(),
            dns1: ArrayString::try_from("8.8.8.8").unwrap(),
            dns2: ArrayString::try_from("8.8.4.4").unwrap(),
        };
        let mut buf = [0u8; 1024];
        let (hdr, len) = build_request(&NetworkCommand::SetIp(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_IP_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<ip>192.168.1.100</ip>"));
        assert!(xml.contains("<dhcp>0</dhcp>"));
    }

    #[test]
    fn build_get_link_type() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&NetworkCommand::GetLinkType, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_LINK_TYPE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<LinkType"));
    }

    #[test]
    fn parse_ip_response() {
        let xml = b"<body>\
            <NetworkGeneral version=\"1.1\">\
                <dhcp>0</dhcp>\
                <ip>10.0.0.50</ip>\
                <mask>255.255.255.0</mask>\
                <gateway>10.0.0.1</gateway>\
                <dns1>1.1.1.1</dns1>\
                <dns2>1.0.0.1</dns2>\
            </NetworkGeneral>\
        </body>";
        let event = parse_response(NetworkResponseKind::Ip, xml).unwrap();
        match event {
            NetworkEvent::Ip(cfg) => {
                assert!(!cfg.dhcp);
                assert_eq!(cfg.ip.as_str(), "10.0.0.50");
                assert_eq!(cfg.mask.as_str(), "255.255.255.0");
                assert_eq!(cfg.gateway.as_str(), "10.0.0.1");
                assert_eq!(cfg.dns1.as_str(), "1.1.1.1");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_link_type_response() {
        let xml = b"<body>\
            <LinkType version=\"1.1\">\
                <type>ethernet</type>\
            </LinkType>\
        </body>";
        let event = parse_response(NetworkResponseKind::LinkType, xml).unwrap();
        match event {
            NetworkEvent::LinkType(info) => {
                assert_eq!(info.link_type.as_str(), "ethernet");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_wifi_signal_response() {
        let xml = b"<body>\
            <WifiSignal version=\"1.1\">\
                <signal>85</signal>\
            </WifiSignal>\
        </body>";
        let event = parse_response(NetworkResponseKind::WifiSignal, xml).unwrap();
        match event {
            NetworkEvent::WifiSignal(info) => {
                assert_eq!(info.signal, 85);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_cellular_info_response() {
        let xml = b"<body>\
            <CellularInfo version=\"1.1\">\
                <signal>70</signal>\
                <networkType>4G</networkType>\
            </CellularInfo>\
        </body>";
        let event = parse_response(NetworkResponseKind::CellularInfo, xml).unwrap();
        match event {
            NetworkEvent::CellularInfo(info) => {
                assert_eq!(info.signal, 70);
                assert_eq!(info.network_type.as_str(), "4G");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_cloud_login_key_response() {
        let xml = b"<body>\
            <CloudLoginKey version=\"1.1\">\
                <key>abc123xyz</key>\
            </CloudLoginKey>\
        </body>";
        let event = parse_response(NetworkResponseKind::CloudLoginKey, xml).unwrap();
        match event {
            NetworkEvent::CloudLoginKey(info) => {
                assert_eq!(info.key.as_str(), "abc123xyz");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn unknown_fields_tolerated() {
        let xml = b"<body>\
            <WifiSignal version=\"1.1\">\
                <signal>50</signal>\
                <futureField>unknown</futureField>\
            </WifiSignal>\
        </body>";
        let event = parse_response(NetworkResponseKind::WifiSignal, xml).unwrap();
        match event {
            NetworkEvent::WifiSignal(info) => assert_eq!(info.signal, 50),
            _ => panic!("wrong event"),
        }
    }
}
