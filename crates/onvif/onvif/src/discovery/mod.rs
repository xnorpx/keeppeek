mod network_enumeration;

use crate::{
    discovery::network_enumeration::enumerate_network_v4,
    utils::{display_list::DisplayList, hash::calculate_hash},
};
use schema::ws_discovery::{probe, probe_matches};
use std::{
    collections::{HashSet, VecDeque},
    fmt::{Debug, Formatter},
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;
use tracing::debug;
use url::Url;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Network error: {0}")]
    Network(#[from] io::Error),

    #[error("(De)serialization error: {0}")]
    Serde(String),

    #[error("Unsupported feature: {0}")]
    Unsupported(String),

    #[error("Discovery worker failed")]
    WorkerFailed,
}

/// How to discover the devices on the network. Officially, only [DiscoveryMode::Multicast] (the
/// default) is supported by all onvif devices. However, it is said that sending unicast packets
/// can work.
#[derive(Debug, Clone)]
pub enum DiscoveryMode {
    /// The normal WS-Discovery Mode
    Multicast,
    /// The unicast approach
    Unicast {
        /// The network IP address. Must be a valid network address, otherwise the behavior
        /// will be undefined
        network: Ipv4Addr,
        /// The network mask, written out in "dotted notation". Must be a valid network mask,
        /// otherwise the behavior will be undefined.
        network_mask: Ipv4Addr,
    },
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct Device {
    /// The WS-Discovery UUID / address reference
    pub address: String,
    pub hardware: Option<String>,
    pub name: Option<String>,
    pub types: Vec<String>,
    pub urls: Vec<Url>,
}

impl Debug for Device {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("name", &self.name)
            .field("url", &DisplayList(&self.urls))
            .field("address", &self.address)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveryBuilder {
    duration: Duration,
    listen_address: IpAddr,
    discovery_mode: DiscoveryMode,
}

impl Default for DiscoveryBuilder {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(5),
            listen_address: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            discovery_mode: DiscoveryMode::Multicast,
        }
    }
}

impl DiscoveryBuilder {
    const LOCAL_PORT: u16 = 0;
    const MULTI_PORT: u16 = 3702;
    const WS_DISCOVERY_BROADCAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
    const MAX_CONCURRENT_SOCK: usize = 32;

    /// How long to listen for the responses from the network.
    pub const fn duration(&mut self, duration: Duration) -> &mut Self {
        self.duration = duration;
        self
    }

    /// Address to listen on.
    ///
    /// By default, it is 0.0.0.0 which is fine for a single-NIC case. With multiple NICs, it's
    /// problematic because 0.0.0.0 is routed to only one NIC, but you may want to run the discovery
    /// on a specific network.
    pub const fn listen_address(&mut self, listen_address: IpAddr) -> &mut Self {
        self.listen_address = listen_address;
        self
    }

    /// Set the discovery mode. See [DiscoveryMode] for a description of how this works.
    /// By default, the multicast mode is chosen.
    pub const fn discovery_mode(&mut self, discovery_mode: DiscoveryMode) -> &mut Self {
        self.discovery_mode = discovery_mode;
        self
    }

    fn discover_unicast(
        &self,
        duration: Duration,
        listen_address: IpAddr,
        network: Ipv4Addr,
        network_mask: Ipv4Addr,
    ) -> Result<Vec<Device>, Error> {
        if matches!(listen_address, IpAddr::V6(_)) {
            return Err(Error::Unsupported("Discovery with IPv6".to_owned()));
        }

        let probe = build_probe();
        let probe_xml = yaserde::ser::to_string(&probe).map_err(Error::Serde)?;
        debug!(
            "Unicast Probe XML: {}. Since you are using unicast, some devices might not be detected",
            probe_xml
        );

        let targets = enumerate_network_v4(network, network_mask);
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        let worker_count = targets.len().min(Self::MAX_CONCURRENT_SOCK);
        let batches = targets.len().div_ceil(worker_count);
        let per_probe_timeout = Duration::from_secs_f64(
            (duration.as_secs_f64() / batches as f64).max(Duration::from_millis(1).as_secs_f64()),
        );
        let jobs = Arc::new(Mutex::new(targets.into_iter().collect::<VecDeque<_>>()));
        let payload = Arc::new(probe_xml.into_bytes());
        let message_id = Arc::new(probe.header.message_id);
        let local_socket_addr = SocketAddr::new(listen_address, Self::LOCAL_PORT);

        let workers = (0..worker_count)
            .map(|_| {
                let jobs = Arc::clone(&jobs);
                let payload = Arc::clone(&payload);
                let message_id = Arc::clone(&message_id);

                thread::Builder::new().spawn(move || {
                    let mut devices = Vec::new();

                    loop {
                        let target = jobs.lock().map_or(None, |mut jobs| jobs.pop_front());
                        let Some(target) = target else {
                            break;
                        };

                        if let Some(device) = probe_unicast(
                            local_socket_addr,
                            target,
                            payload.as_slice(),
                            message_id.as_str(),
                            per_probe_timeout,
                        ) {
                            devices.push(device);
                        }
                    }

                    devices
                })
            })
            .collect::<Result<Vec<_>, io::Error>>()?;

        let mut devices = Vec::new();
        for worker in workers {
            let mut found = worker.join().map_err(|_| Error::WorkerFailed)?;
            devices.append(&mut found);
        }

        Ok(devices)
    }

    fn discover_multicast(
        &self,
        duration: Duration,
        listen_address: IpAddr,
    ) -> Result<Vec<Device>, Error> {
        let IpAddr::V4(listen_address) = listen_address else {
            return Err(Error::Unsupported("Discovery with IPv6".to_owned()));
        };

        let probe = build_probe();
        let probe_xml = yaserde::ser::to_string(&probe).map_err(Error::Serde)?;
        debug!("Probe XML: {probe_xml}");

        let socket = UdpSocket::bind(SocketAddr::new(
            IpAddr::V4(listen_address),
            Self::LOCAL_PORT,
        ))?;
        socket.join_multicast_v4(&Self::WS_DISCOVERY_BROADCAST_ADDR, &listen_address)?;
        socket.send_to(
            probe_xml.as_bytes(),
            SocketAddr::new(
                IpAddr::V4(Self::WS_DISCOVERY_BROADCAST_ADDR),
                Self::MULTI_PORT,
            ),
        )?;

        let deadline = Instant::now() + duration;
        let mut known_responses = HashSet::new();
        let mut devices = Vec::new();

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            socket.set_read_timeout(Some(remaining))?;

            match recv_string(&socket) {
                Ok((xml, source)) => {
                    if !known_responses.insert(calculate_hash(&xml)) {
                        debug!("Duplicate response from {source}, skipping ...");
                        continue;
                    }

                    if let Some(device) = device_from_probe_response(&xml, &probe.header.message_id)
                    {
                        debug!("Found device {device:?}");
                        devices.push(device);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    break;
                }
                Err(error) => return Err(Error::Network(error)),
            }
        }

        Ok(devices)
    }

    /// Discovers devices on a local network using blocking WS-Discovery.
    ///
    /// This method blocks for up to the configured duration while collecting
    /// multicast or unicast probe responses.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use onvif::discovery;
    ///
    /// let devices = discovery::DiscoveryBuilder::default().discover().unwrap();
    /// for device in devices {
    ///     println!("Device found: {device:?}");
    /// }
    /// ```
    pub fn discover(&self) -> Result<Vec<Device>, Error> {
        match &self.discovery_mode {
            DiscoveryMode::Multicast => self.discover_multicast(self.duration, self.listen_address),
            DiscoveryMode::Unicast {
                network,
                network_mask,
            } => self.discover_unicast(self.duration, self.listen_address, *network, *network_mask),
        }
    }
}

fn probe_unicast(
    local_socket_addr: SocketAddr,
    target: Ipv4Addr,
    payload: &[u8],
    message_id: &str,
    timeout: Duration,
) -> Option<Device> {
    let socket = UdpSocket::bind(local_socket_addr).ok()?;
    socket.set_read_timeout(Some(timeout)).ok()?;
    socket
        .send_to(
            payload,
            SocketAddr::new(IpAddr::V4(target), DiscoveryBuilder::MULTI_PORT),
        )
        .ok()?;
    let (xml, source) = recv_string(&socket).ok()?;
    debug!("Probe match XML from {source}: {xml}");

    device_from_probe_response(&xml, message_id)
}

fn recv_string(s: &UdpSocket) -> io::Result<(String, SocketAddr)> {
    let mut buf = vec![0; 16 * 1024];
    let (len, src) = s.recv_from(&mut buf)?;

    Ok((String::from_utf8_lossy(&buf[..len]).to_string(), src))
}

fn device_from_probe_response(xml: &str, message_id: &str) -> Option<Device> {
    let envelope = match yaserde::de::from_str::<probe_matches::Envelope>(xml) {
        Ok(envelope) => envelope,
        Err(error) => {
            debug!("Deserialization failed: {error}");
            return None;
        }
    };

    if envelope.header.relates_to != message_id {
        debug!("Unrelated message");
        return None;
    }

    device_from_envelope(envelope)
}

fn device_from_envelope(envelope: probe_matches::Envelope) -> Option<Device> {
    let onvif_probe_match = envelope
        .body
        .probe_matches
        .probe_match
        .iter()
        .find(|probe_match| {
            probe_match
                .find_in_scopes("onvif://www.onvif.org")
                .is_some()
        })?;

    let name = onvif_probe_match.name();
    let urls = onvif_probe_match.x_addrs();
    let hardware = onvif_probe_match.hardware();
    let address = onvif_probe_match.endpoint_reference_address();
    let types = onvif_probe_match
        .types()
        .into_iter()
        .map(Into::into)
        .collect();

    Some(Device {
        name,
        urls,
        address,
        hardware,
        types,
    })
}

fn build_probe() -> probe::Envelope {
    use probe::*;

    Envelope {
        header: Header {
            message_id: format!("uuid:{}", uuid::Uuid::new_v4()),
            action: "http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe".into(),
            to: "urn:schemas-xmlsoap-org:ws:2005:04:discovery".into(),
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test serves more as an example of how the unicast discovery works.
    #[test]
    #[ignore = "requires a reachable ONVIF device"]
    fn test_unicast() {
        let devices = DiscoveryBuilder::default()
            .discovery_mode(DiscoveryMode::Unicast {
                network: Ipv4Addr::new(192, 168, 1, 0),
                network_mask: Ipv4Addr::new(255, 255, 255, 0),
            })
            .discover()
            .unwrap();

        println!("Devices found: {devices:?}");
    }

    #[test]
    fn test_xaddrs_extraction() {
        const DEVICE_ADDRESS: &str = "an address";

        let make_xml = |relates_to: &str, xaddrs: &str| -> String {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
            <SOAP-ENV:Envelope
                        xmlns:SOAP-ENV="http://www.w3.org/2003/05/soap-envelope"
                        xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing"
                        xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery"
                        xmlns:dn="http://www.onvif.org/ver10/network/wsdl">
                <SOAP-ENV:Header>
                    <wsa:RelatesTo>{relates_to}</wsa:RelatesTo>
                </SOAP-ENV:Header>
                <SOAP-ENV:Body>
                    <d:ProbeMatches>
                        <d:ProbeMatch>
                            <d:XAddrs>http://something.else</d:XAddrs>
                        </d:ProbeMatch>
                        <d:ProbeMatch>
                            <wsa:EndpointReference>
                                <wsa:Address>{DEVICE_ADDRESS}</wsa:Address>
                            </wsa:EndpointReference>
                            <d:Scopes>onvif://www.onvif.org/name/MyCamera2000</d:Scopes>
                            <d:XAddrs>{xaddrs}</d:XAddrs>
                        </d:ProbeMatch>
                    </d:ProbeMatches>
                </SOAP-ENV:Body>
            </SOAP-ENV:Envelope>
            "#
            )
        };

        let our_uuid = "uuid:84ede3de-7dec-11d0-c360-F01234567890";
        let bad_uuid = "uuid:84ede3de-7dec-11d0-c360-F00000000000";

        let input = [
            make_xml(our_uuid, "http://addr_20 http://addr_21 http://addr_22"),
            make_xml(bad_uuid, "http://addr_30 http://addr_31"),
        ];

        let actual = input
            .iter()
            .filter_map(|xml| yaserde::de::from_str::<probe_matches::Envelope>(xml).ok())
            .filter(|envelope| envelope.header.relates_to == our_uuid)
            .filter_map(device_from_envelope)
            .collect::<Vec<_>>();

        assert_eq!(actual.len(), 1);

        // OK: message UUID matches and addr responds
        assert_eq!(
            actual,
            &[Device {
                urls: vec![
                    Url::parse("http://addr_20").unwrap(),
                    Url::parse("http://addr_21").unwrap(),
                    Url::parse("http://addr_22").unwrap(),
                ],
                name: Some("MyCamera2000".to_string()),
                hardware: None,
                address: DEVICE_ADDRESS.to_string(),
                types: vec![],
            }]
        );
    }
}
