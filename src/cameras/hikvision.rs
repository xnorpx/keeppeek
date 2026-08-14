use super::{DiscoveredCamera, network};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    sync::Mutex,
    thread,
    time::Duration,
};

const SADP_MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SADP_PORT: u16 = 37_020;
const MAX_PACKET_SIZE: usize = 65_535;

#[derive(Debug, Default, Deserialize)]
#[serde(rename = "ProbeMatch")]
struct ProbeMatch {
    #[serde(rename = "DeviceType", default)]
    device_type: String,
    #[serde(rename = "DeviceDescription", default)]
    device_description: String,
    #[serde(rename = "DeviceSN", default)]
    serial_number: String,
    #[serde(rename = "MAC", default)]
    mac: String,
    #[serde(rename = "IPv4Address", default)]
    ipv4_address: String,
    #[serde(rename = "DHCP", default)]
    dhcp: String,
    #[serde(rename = "CommandPort", default)]
    command_port: u16,
    #[serde(rename = "HttpPort", default)]
    http_port: u16,
    #[serde(rename = "SoftwareVersion", default)]
    software_version: String,
    #[serde(rename = "Activated", default)]
    activated: String,
}

#[derive(Debug)]
struct SadpDevice {
    ip: Ipv4Addr,
    device_type: Option<String>,
    description: Option<String>,
    serial_number: Option<String>,
    mac: Option<String>,
    dhcp: Option<bool>,
    command_port: Option<u16>,
    http_port: Option<u16>,
    software_version: Option<String>,
    activated: Option<bool>,
}

pub(super) fn discover(duration: Duration) -> anyhow::Result<Vec<DiscoveredCamera>> {
    let networks = network::local_networks()?;
    let devices = Mutex::new(Vec::new());

    thread::scope(|scope| {
        for local in &networks {
            let devices = &devices;
            scope.spawn(move || match discover_on_interface(local, duration) {
                Ok(found) => match devices.lock() {
                    Ok(mut devices) => devices.extend(found),
                    Err(poisoned) => poisoned.into_inner().extend(found),
                },
                Err(error) => tracing::debug!(
                    interface = local.interface_name,
                    ip = %local.interface_ip,
                    %error,
                    "Hikvision SADP discovery failed on interface"
                ),
            });
        }
    });

    let devices = match devices.into_inner() {
        Ok(devices) => devices,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut unique = HashMap::new();
    for device in devices {
        let key = device.mac.clone().unwrap_or_else(|| device.ip.to_string());
        unique
            .entry(key)
            .and_modify(|existing| merge_device(existing, &device))
            .or_insert(device);
    }

    let mut cameras = unique
        .into_values()
        .map(|device| {
            tracing::info!(
                ip = %device.ip,
                model = device.device_type.as_deref().unwrap_or("unknown"),
                command_port = device.command_port,
                http_port = device.http_port,
                dhcp = device.dhcp,
                activated = device.activated,
                firmware = device.software_version.as_deref().unwrap_or("unknown"),
                serial_present = device.serial_number.is_some(),
                "Hikvision SADP device discovered"
            );
            DiscoveredCamera {
                ip: IpAddr::V4(device.ip),
                brand: "hikvision",
                name: device.description,
                model: device.device_type,
                onvif_urls: Vec::new(),
                sources: vec!["sadp"],
            }
        })
        .collect::<Vec<_>>();
    cameras.sort_unstable_by_key(|camera| camera.ip);
    Ok(cameras)
}

fn discover_on_interface(
    local: &network::LocalNetwork,
    duration: Duration,
) -> anyhow::Result<Vec<SadpDevice>> {
    let socket = UdpSocket::bind(SocketAddrV4::new(local.interface_ip, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(duration))?;

    let uuid = probe_uuid();
    let probes = [
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><Probe><Uuid>{uuid}</Uuid><Types>inquiry</Types></Probe>"
        ),
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><Probe><Uuid>{uuid}</Uuid><Types>inquiry_v32</Types></Probe>"
        ),
    ];
    let destinations = HashSet::from([
        SocketAddrV4::new(SADP_MULTICAST, SADP_PORT),
        SocketAddrV4::new(Ipv4Addr::BROADCAST, SADP_PORT),
        SocketAddrV4::new(local.broadcast, SADP_PORT),
    ]);
    for probe in &probes {
        for destination in &destinations {
            if let Err(error) = socket.send_to(probe.as_bytes(), destination) {
                tracing::debug!(
                    interface = local.interface_name,
                    %destination,
                    %error,
                    "unable to send Hikvision SADP probe"
                );
            }
        }
    }

    let mut devices = Vec::new();
    let mut buffer = vec![0; MAX_PACKET_SIZE];
    loop {
        match socket.recv_from(&mut buffer) {
            Ok((length, remote)) => {
                let Ok(response) = std::str::from_utf8(&buffer[..length]) else {
                    continue;
                };
                if let Some(device) = parse_response(response, remote) {
                    devices.push(device);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(devices)
}

fn parse_response(response: &str, remote: SocketAddr) -> Option<SadpDevice> {
    if !response.contains("ProbeMatch") {
        return None;
    }
    let response = response.trim_matches(char::from(0));
    let probe = quick_xml::de::from_str::<ProbeMatch>(response).ok()?;
    let advertised_ip = probe.ipv4_address.parse::<Ipv4Addr>().ok();
    let ip = advertised_ip
        .filter(|ip| !ip.is_unspecified())
        .or_else(|| match remote.ip() {
            IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
            _ => None,
        })?;
    Some(SadpDevice {
        ip,
        device_type: nonempty(probe.device_type),
        description: nonempty(probe.device_description),
        serial_number: nonempty(probe.serial_number),
        mac: nonempty(probe.mac).map(normalize_mac),
        dhcp: parse_bool(&probe.dhcp),
        command_port: (probe.command_port > 0).then_some(probe.command_port),
        http_port: (probe.http_port > 0).then_some(probe.http_port),
        software_version: nonempty(probe.software_version),
        activated: parse_bool(&probe.activated),
    })
}

fn merge_device(existing: &mut SadpDevice, new: &SadpDevice) {
    existing.device_type = existing
        .device_type
        .take()
        .or_else(|| new.device_type.clone());
    existing.description = existing
        .description
        .take()
        .or_else(|| new.description.clone());
    existing.serial_number = existing
        .serial_number
        .take()
        .or_else(|| new.serial_number.clone());
    existing.mac = existing.mac.take().or_else(|| new.mac.clone());
    existing.dhcp = existing.dhcp.or(new.dhcp);
    existing.command_port = existing.command_port.or(new.command_port);
    existing.http_port = existing.http_port.or(new.http_port);
    existing.software_version = existing
        .software_version
        .take()
        .or_else(|| new.software_version.clone());
    existing.activated = existing.activated.or(new.activated);
}

fn probe_uuid() -> String {
    let value = format!("{:032x}", rand::random::<u128>());
    format!(
        "{}-{}-{}-{}-{}",
        &value[0..8],
        &value[8..12],
        &value[12..16],
        &value[16..20],
        &value[20..32]
    )
}

fn nonempty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn normalize_mac(mac: String) -> String {
    mac.trim().replace('-', ":").to_ascii_uppercase()
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESPONSE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ProbeMatch>
  <Uuid>device-id</Uuid>
  <Types>inquiry</Types>
  <DeviceType>DS-2CD2043G2-I</DeviceType>
  <DeviceDescription>Network Camera</DeviceDescription>
  <DeviceSN>serial</DeviceSN>
  <MAC>44-19-B6-00-00-01</MAC>
  <IPv4Address>192.168.6.210</IPv4Address>
  <DHCP>true</DHCP>
  <CommandPort>8000</CommandPort>
  <HttpPort>80</HttpPort>
  <SoftwareVersion>V5.7.0</SoftwareVersion>
  <Activated>true</Activated>
</ProbeMatch>"#;

    #[test]
    fn parses_hikvision_probe_match() {
        let device = parse_response(
            RESPONSE,
            SocketAddr::from((Ipv4Addr::new(192, 168, 6, 210), SADP_PORT)),
        )
        .unwrap();

        assert_eq!(device.ip, Ipv4Addr::new(192, 168, 6, 210));
        assert_eq!(device.device_type.as_deref(), Some("DS-2CD2043G2-I"));
        assert_eq!(device.description.as_deref(), Some("Network Camera"));
        assert_eq!(device.mac.as_deref(), Some("44:19:B6:00:00:01"));
        assert_eq!(device.command_port, Some(8_000));
        assert_eq!(device.http_port, Some(80));
        assert_eq!(device.dhcp, Some(true));
        assert_eq!(device.activated, Some(true));
    }

    #[test]
    fn uses_sender_ip_when_advertised_ip_is_unspecified() {
        let response = RESPONSE.replace("192.168.6.210", "0.0.0.0");
        let device = parse_response(
            &response,
            SocketAddr::from((Ipv4Addr::new(192, 168, 6, 211), SADP_PORT)),
        )
        .unwrap();

        assert_eq!(device.ip, Ipv4Addr::new(192, 168, 6, 211));
    }
}
