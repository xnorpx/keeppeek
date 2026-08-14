//! SANS-I/O Baichuan UDP packet framing and reliable payload transport.

use crate::BcError;
use std::{
    collections::{BTreeMap, VecDeque},
    io::Cursor,
    time::{Duration, Instant},
};
use xml::{
    reader::{EventReader, XmlEvent as ReadEvent},
    writer::{EmitterConfig, XmlEvent as WriteEvent},
};

const DISCOVERY_MAGIC: u32 = 0x2a87_cf3a;
const ACK_MAGIC: u32 = 0x2a87_cf20;
const DATA_MAGIC: u32 = 0x2a87_cf10;
const DATA_HEADER_LEN: usize = 20;
const ACK_HEADER_LEN: usize = 28;
const DISCOVERY_HEADER_LEN: usize = 20;
const XML_KEY: [u32; 8] = [
    0x1f2d_3c4b,
    0x5a6c_7f8d,
    0x3817_2e4b,
    0x8271_635a,
    0x863f_1a2b,
    0xa5c6_f7d8,
    0x8371_e1b4,
    0x17f2_d3a5,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpDiscovery {
    pub transmission_id: u32,
    pub xml: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpAck {
    pub connection_id: i32,
    pub group_id: u32,
    pub packet_id: u32,
    pub latency: u32,
    pub received: Vec<u8>,
}

impl UdpAck {
    pub const fn empty(connection_id: i32) -> Self {
        Self {
            connection_id,
            group_id: u32::MAX,
            packet_id: u32::MAX,
            latency: 0,
            received: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpData {
    pub connection_id: i32,
    pub packet_id: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BcUdpPacket {
    Discovery(UdpDiscovery),
    Ack(UdpAck),
    Data(UdpData),
}

#[derive(Debug, Clone)]
pub struct BcUdpDiscoveryConfig {
    pub uid: String,
    pub client_id: i32,
    pub client_port: u16,
    pub transmission_id: u32,
    pub mtu: u32,
    pub retry_interval: Duration,
}

impl BcUdpDiscoveryConfig {
    pub fn new(uid: impl Into<String>, client_id: i32, client_port: u16) -> Self {
        Self {
            uid: uid.into(),
            client_id,
            client_port,
            transmission_id: 1,
            mtu: 1_350,
            retry_interval: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BcUdpConnection {
    pub client_id: i32,
    pub camera_id: i32,
    pub transmission_id: u32,
}

impl BcUdpConnection {
    pub fn transport(self, now: Instant, config: BcUdpConfig) -> Result<BcUdpTransport, BcError> {
        BcUdpTransport::new(self.client_id, self.camera_id, now, config)
    }

    pub fn heartbeat(self) -> Result<Vec<u8>, BcError> {
        discovery_xml_packet(
            self.transmission_id,
            "C2D_HB",
            &[
                ("cid", self.client_id.to_string()),
                ("did", self.camera_id.to_string()),
            ],
        )
    }

    pub fn disconnect(self) -> Result<Vec<u8>, BcError> {
        discovery_xml_packet(
            self.transmission_id,
            "C2D_DISC",
            &[
                ("cid", self.client_id.to_string()),
                ("did", self.camera_id.to_string()),
            ],
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BcUdpDiscoveryOutput {
    Datagram(Vec<u8>),
    Connected(BcUdpConnection),
    Timeout(Instant),
}

pub struct BcUdpDiscovery {
    config: BcUdpDiscoveryConfig,
    request: Vec<u8>,
    connection: Option<BcUdpConnection>,
    next_send: Instant,
}

impl BcUdpDiscovery {
    pub fn new(config: BcUdpDiscoveryConfig, now: Instant) -> Result<Self, BcError> {
        if config.uid.is_empty() {
            return Err(BcError::InvalidUdpPacket("camera UID is empty"));
        }
        if config.client_port == 0 {
            return Err(BcError::InvalidUdpPacket("client port is zero"));
        }
        if config.retry_interval.is_zero() {
            return Err(BcError::InvalidUdpPacket(
                "discovery retry interval must be non-zero",
            ));
        }
        let request = discovery_xml_packet(
            config.transmission_id,
            "C2D_C",
            &[
                ("uid", config.uid.clone()),
                ("cli.port", config.client_port.to_string()),
                ("cid", config.client_id.to_string()),
                ("mtu", config.mtu.to_string()),
                ("debug", "0".to_owned()),
                ("p", "MAC".to_owned()),
            ],
        )?;
        Ok(Self {
            config,
            request,
            connection: None,
            next_send: now,
        })
    }

    pub fn handle_datagram(&mut self, datagram: &[u8]) -> Result<(), BcError> {
        let BcUdpPacket::Discovery(discovery) = BcUdpPacket::decode(datagram)? else {
            return Ok(());
        };
        let Some((response, client_id, camera_id)) = parse_connect_reply(&discovery.xml)? else {
            return Ok(());
        };
        if response != 0 || client_id != self.config.client_id {
            return Ok(());
        }
        self.connection = Some(BcUdpConnection {
            client_id,
            camera_id,
            transmission_id: discovery.transmission_id,
        });
        Ok(())
    }

    pub fn poll_output(&mut self, now: Instant) -> BcUdpDiscoveryOutput {
        if let Some(connection) = self.connection.take() {
            return BcUdpDiscoveryOutput::Connected(connection);
        }
        if now >= self.next_send {
            advance_deadline(&mut self.next_send, self.config.retry_interval, now);
            return BcUdpDiscoveryOutput::Datagram(self.request.clone());
        }
        BcUdpDiscoveryOutput::Timeout(self.next_send)
    }
}

impl BcUdpPacket {
    pub fn decode(datagram: &[u8]) -> Result<Self, BcError> {
        let magic = read_u32(datagram, 0)?;
        match magic {
            DISCOVERY_MAGIC => decode_discovery(datagram),
            ACK_MAGIC => decode_ack(datagram),
            DATA_MAGIC => decode_data(datagram),
            _ => Err(BcError::InvalidUdpPacket("unknown magic")),
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, BcError> {
        match self {
            Self::Discovery(discovery) => encode_discovery(discovery),
            Self::Ack(ack) => encode_ack(ack),
            Self::Data(data) => encode_data(data),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BcUdpConfig {
    pub mtu: usize,
    pub ack_interval: Duration,
    pub resend_interval: Duration,
}

impl Default for BcUdpConfig {
    fn default() -> Self {
        Self {
            mtu: 1_350,
            ack_interval: Duration::from_millis(10),
            resend_interval: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BcUdpOutput {
    Datagram(Vec<u8>),
    Payload(Vec<u8>),
    Timeout(Instant),
}

pub struct BcUdpTransport {
    client_id: i32,
    camera_id: i32,
    config: BcUdpConfig,
    next_send_packet: u32,
    next_receive_packet: u32,
    sent: BTreeMap<u32, UdpData>,
    received: BTreeMap<u32, Vec<u8>>,
    outputs: VecDeque<BcUdpOutput>,
    next_ack: Instant,
    next_resend: Instant,
}

impl BcUdpTransport {
    pub fn new(
        client_id: i32,
        camera_id: i32,
        now: Instant,
        config: BcUdpConfig,
    ) -> Result<Self, BcError> {
        if config.mtu <= DATA_HEADER_LEN {
            return Err(BcError::InvalidUdpPacket("MTU is too small"));
        }
        if config.ack_interval.is_zero() || config.resend_interval.is_zero() {
            return Err(BcError::InvalidUdpPacket(
                "ACK and resend intervals must be non-zero",
            ));
        }
        Ok(Self {
            client_id,
            camera_id,
            next_send_packet: 0,
            next_receive_packet: 0,
            sent: BTreeMap::new(),
            received: BTreeMap::new(),
            outputs: VecDeque::new(),
            next_ack: now + config.ack_interval,
            next_resend: now + config.resend_interval,
            config,
        })
    }

    pub fn queue_payload(&mut self, payload: &[u8]) -> Result<(), BcError> {
        let chunk_size = self.config.mtu - DATA_HEADER_LEN;
        for chunk in payload.chunks(chunk_size) {
            let data = UdpData {
                connection_id: self.camera_id,
                packet_id: self.next_send_packet,
                payload: chunk.to_vec(),
            };
            self.next_send_packet = self.next_send_packet.wrapping_add(1);
            self.outputs.push_back(BcUdpOutput::Datagram(
                BcUdpPacket::Data(data.clone()).encode()?,
            ));
            self.sent.insert(data.packet_id, data);
        }
        Ok(())
    }

    pub fn handle_datagram(&mut self, datagram: &[u8]) -> Result<(), BcError> {
        match BcUdpPacket::decode(datagram)? {
            BcUdpPacket::Discovery(_) => {}
            BcUdpPacket::Ack(ack) if ack.connection_id == self.client_id => {
                self.handle_ack(&ack);
            }
            BcUdpPacket::Ack(_) => {}
            BcUdpPacket::Data(data) if data.connection_id == self.client_id => {
                if data.packet_id >= self.next_receive_packet {
                    self.received.entry(data.packet_id).or_insert(data.payload);
                    self.flush_received();
                }
            }
            BcUdpPacket::Data(_) => {}
        }
        Ok(())
    }

    pub fn poll_output(&mut self, now: Instant) -> Result<BcUdpOutput, BcError> {
        if let Some(output) = self.outputs.pop_front() {
            return Ok(output);
        }
        if now >= self.next_ack {
            advance_deadline(&mut self.next_ack, self.config.ack_interval, now);
            let ack = self.build_ack();
            return Ok(BcUdpOutput::Datagram(BcUdpPacket::Ack(ack).encode()?));
        }
        if now >= self.next_resend {
            advance_deadline(&mut self.next_resend, self.config.resend_interval, now);
            for data in self.sent.values() {
                self.outputs.push_back(BcUdpOutput::Datagram(
                    BcUdpPacket::Data(data.clone()).encode()?,
                ));
            }
            if let Some(output) = self.outputs.pop_front() {
                return Ok(output);
            }
        }
        Ok(BcUdpOutput::Timeout(self.next_ack.min(self.next_resend)))
    }

    pub fn pending_send_packets(&self) -> usize {
        self.sent.len()
    }

    fn flush_received(&mut self) {
        let mut payload = Vec::new();
        while let Some(chunk) = self.received.remove(&self.next_receive_packet) {
            payload.extend_from_slice(&chunk);
            self.next_receive_packet = self.next_receive_packet.wrapping_add(1);
        }
        if !payload.is_empty() {
            self.outputs.push_back(BcUdpOutput::Payload(payload));
        }
    }

    fn build_ack(&self) -> UdpAck {
        if self.next_receive_packet == 0 {
            return UdpAck::empty(self.camera_id);
        }
        let mut received = Vec::new();
        if let Some(last) = self.received.keys().next_back().copied() {
            for packet_id in self.next_receive_packet..=last {
                received.push(u8::from(self.received.contains_key(&packet_id)));
            }
        }
        UdpAck {
            connection_id: self.camera_id,
            group_id: 0,
            packet_id: self.next_receive_packet.wrapping_sub(1),
            latency: 0,
            received,
        }
    }

    fn handle_ack(&mut self, ack: &UdpAck) {
        if ack.packet_id == u32::MAX {
            return;
        }
        self.sent.retain(|packet_id, _| *packet_id > ack.packet_id);
        for (offset, received) in ack.received.iter().copied().enumerate() {
            if received != 0 {
                let packet_id = ack.packet_id.wrapping_add(1).wrapping_add(offset as u32);
                self.sent.remove(&packet_id);
            }
        }
    }
}

fn decode_discovery(datagram: &[u8]) -> Result<BcUdpPacket, BcError> {
    let payload_len = read_len(datagram, 4)?;
    ensure_datagram_len(datagram, DISCOVERY_HEADER_LEN, payload_len)?;
    if read_u32(datagram, 8)? != 1 {
        return Err(BcError::InvalidUdpPacket("invalid discovery marker"));
    }
    let transmission_id = read_u32(datagram, 12)?;
    let expected = read_u32(datagram, 16)?;
    let encrypted = &datagram[DISCOVERY_HEADER_LEN..];
    let actual = udp_crc(encrypted);
    if actual != expected {
        return Err(BcError::UdpChecksumMismatch { expected, actual });
    }
    Ok(BcUdpPacket::Discovery(UdpDiscovery {
        transmission_id,
        xml: crypt_xml(transmission_id, encrypted),
    }))
}

fn decode_ack(datagram: &[u8]) -> Result<BcUdpPacket, BcError> {
    let payload_len = read_len(datagram, 24)?;
    ensure_datagram_len(datagram, ACK_HEADER_LEN, payload_len)?;
    if read_u32(datagram, 8)? != 0 {
        return Err(BcError::InvalidUdpPacket("invalid ACK marker"));
    }
    Ok(BcUdpPacket::Ack(UdpAck {
        connection_id: read_i32(datagram, 4)?,
        group_id: read_u32(datagram, 12)?,
        packet_id: read_u32(datagram, 16)?,
        latency: read_u32(datagram, 20)?,
        received: datagram[ACK_HEADER_LEN..].to_vec(),
    }))
}

fn decode_data(datagram: &[u8]) -> Result<BcUdpPacket, BcError> {
    let payload_len = read_len(datagram, 16)?;
    ensure_datagram_len(datagram, DATA_HEADER_LEN, payload_len)?;
    if read_u32(datagram, 8)? != 0 {
        return Err(BcError::InvalidUdpPacket("invalid data marker"));
    }
    Ok(BcUdpPacket::Data(UdpData {
        connection_id: read_i32(datagram, 4)?,
        packet_id: read_u32(datagram, 12)?,
        payload: datagram[DATA_HEADER_LEN..].to_vec(),
    }))
}

fn encode_discovery(discovery: &UdpDiscovery) -> Result<Vec<u8>, BcError> {
    let payload_len = u32::try_from(discovery.xml.len())
        .map_err(|_| BcError::InvalidUdpPacket("discovery payload is too large"))?;
    let encrypted = crypt_xml(discovery.transmission_id, &discovery.xml);
    let mut datagram = Vec::with_capacity(DISCOVERY_HEADER_LEN + encrypted.len());
    push_u32(&mut datagram, DISCOVERY_MAGIC);
    push_u32(&mut datagram, payload_len);
    push_u32(&mut datagram, 1);
    push_u32(&mut datagram, discovery.transmission_id);
    push_u32(&mut datagram, udp_crc(&encrypted));
    datagram.extend_from_slice(&encrypted);
    Ok(datagram)
}

fn encode_ack(ack: &UdpAck) -> Result<Vec<u8>, BcError> {
    let payload_len = u32::try_from(ack.received.len())
        .map_err(|_| BcError::InvalidUdpPacket("ACK payload is too large"))?;
    let mut datagram = Vec::with_capacity(ACK_HEADER_LEN + ack.received.len());
    push_u32(&mut datagram, ACK_MAGIC);
    push_i32(&mut datagram, ack.connection_id);
    push_u32(&mut datagram, 0);
    push_u32(&mut datagram, ack.group_id);
    push_u32(&mut datagram, ack.packet_id);
    push_u32(&mut datagram, ack.latency);
    push_u32(&mut datagram, payload_len);
    datagram.extend_from_slice(&ack.received);
    Ok(datagram)
}

fn encode_data(data: &UdpData) -> Result<Vec<u8>, BcError> {
    let payload_len = u32::try_from(data.payload.len())
        .map_err(|_| BcError::InvalidUdpPacket("data payload is too large"))?;
    let mut datagram = Vec::with_capacity(DATA_HEADER_LEN + data.payload.len());
    push_u32(&mut datagram, DATA_MAGIC);
    push_i32(&mut datagram, data.connection_id);
    push_u32(&mut datagram, 0);
    push_u32(&mut datagram, data.packet_id);
    push_u32(&mut datagram, payload_len);
    datagram.extend_from_slice(&data.payload);
    Ok(datagram)
}

fn crypt_xml(transmission_id: u32, payload: &[u8]) -> Vec<u8> {
    let key = XML_KEY
        .iter()
        .flat_map(|word| word.wrapping_add(transmission_id).to_le_bytes())
        .cycle();
    payload
        .iter()
        .copied()
        .zip(key)
        .map(|(byte, key)| byte ^ key)
        .collect()
}

fn discovery_xml_packet(
    transmission_id: u32,
    element: &str,
    fields: &[(&str, String)],
) -> Result<Vec<u8>, BcError> {
    let mut xml = Vec::new();
    let config = EmitterConfig::new()
        .write_document_declaration(false)
        .perform_indent(false);
    let mut writer = config.create_writer(Cursor::new(&mut xml));
    writer
        .write(WriteEvent::start_element("P2P"))
        .map_err(|_| BcError::XmlParse("failed to write UDP discovery XML"))?;
    writer
        .write(WriteEvent::start_element(element))
        .map_err(|_| BcError::XmlParse("failed to write UDP discovery XML"))?;
    for (name, value) in fields {
        if let Some((parent, child)) = name.split_once('.') {
            writer
                .write(WriteEvent::start_element(parent))
                .map_err(|_| BcError::XmlParse("failed to write UDP discovery XML"))?;
            write_text_element(&mut writer, child, value)?;
            writer
                .write(WriteEvent::end_element())
                .map_err(|_| BcError::XmlParse("failed to write UDP discovery XML"))?;
        } else {
            write_text_element(&mut writer, name, value)?;
        }
    }
    writer
        .write(WriteEvent::end_element())
        .and_then(|()| writer.write(WriteEvent::end_element()))
        .map_err(|_| BcError::XmlParse("failed to write UDP discovery XML"))?;
    drop(writer);
    BcUdpPacket::Discovery(UdpDiscovery {
        transmission_id,
        xml,
    })
    .encode()
}

fn write_text_element<W: std::io::Write>(
    writer: &mut xml::writer::EventWriter<W>,
    name: &str,
    value: &str,
) -> Result<(), BcError> {
    writer
        .write(WriteEvent::start_element(name))
        .and_then(|()| writer.write(WriteEvent::characters(value)))
        .and_then(|()| writer.write(WriteEvent::end_element()))
        .map_err(|_| BcError::XmlParse("failed to write UDP discovery XML"))
}

fn parse_connect_reply(xml: &[u8]) -> Result<Option<(i32, i32, i32)>, BcError> {
    let mut in_reply = false;
    let mut current = None;
    let mut response = None;
    let mut client_id = None;
    let mut camera_id = None;
    for event in EventReader::new(xml) {
        match event {
            Ok(ReadEvent::StartElement { name, .. }) => {
                if name.local_name == "D2C_C_R" {
                    in_reply = true;
                }
                current = in_reply.then_some(name.local_name);
            }
            Ok(ReadEvent::Characters(value)) | Ok(ReadEvent::CData(value)) if in_reply => {
                match current.as_deref() {
                    Some("rsp") => response = value.parse().ok(),
                    Some("cid") => client_id = value.parse().ok(),
                    Some("did") => camera_id = value.parse().ok(),
                    _ => {}
                }
            }
            Ok(ReadEvent::EndElement { name }) => {
                if name.local_name == "D2C_C_R" {
                    in_reply = false;
                }
                current = None;
            }
            Ok(ReadEvent::EndDocument) => break,
            Err(_) => return Err(BcError::XmlParse("malformed UDP discovery XML")),
            _ => {}
        }
    }
    Ok(response
        .zip(client_id)
        .zip(camera_id)
        .map(|((response, client_id), camera_id)| (response, client_id, camera_id)))
}

fn udp_crc(payload: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new_with_initial(u32::MAX);
    hasher.update(payload);
    hasher.finalize() ^ u32::MAX
}

fn read_len(datagram: &[u8], offset: usize) -> Result<usize, BcError> {
    usize::try_from(read_u32(datagram, offset)?)
        .map_err(|_| BcError::InvalidUdpPacket("payload length is too large"))
}

fn read_u32(datagram: &[u8], offset: usize) -> Result<u32, BcError> {
    let bytes = datagram
        .get(offset..offset + 4)
        .ok_or(BcError::InvalidUdpPacket("truncated header"))?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("four-byte slice"),
    ))
}

fn read_i32(datagram: &[u8], offset: usize) -> Result<i32, BcError> {
    Ok(i32::from_le_bytes(
        read_u32(datagram, offset)?.to_le_bytes(),
    ))
}

const fn ensure_datagram_len(
    datagram: &[u8],
    header_len: usize,
    payload_len: usize,
) -> Result<(), BcError> {
    if datagram.len() != header_len.saturating_add(payload_len) {
        return Err(BcError::InvalidUdpPacket("payload length mismatch"));
    }
    Ok(())
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn advance_deadline(deadline: &mut Instant, interval: Duration, now: Instant) {
    while *deadline <= now {
        *deadline += interval;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_roundtrips() {
        for packet in [
            BcUdpPacket::Discovery(UdpDiscovery {
                transmission_id: 87,
                xml: b"<P2P><C2D_HB/></P2P>".to_vec(),
            }),
            BcUdpPacket::Ack(UdpAck {
                connection_id: -42,
                group_id: 0,
                packet_id: 9,
                latency: 55_062,
                received: vec![1, 0, 1],
            }),
            BcUdpPacket::Data(UdpData {
                connection_id: 82000,
                packet_id: 2439,
                payload: b"baichuan".to_vec(),
            }),
        ] {
            assert_eq!(
                BcUdpPacket::decode(&packet.encode().unwrap()).unwrap(),
                packet
            );
        }
    }

    #[test]
    fn discovery_checksum_is_validated() {
        let mut datagram = BcUdpPacket::Discovery(UdpDiscovery {
            transmission_id: 1,
            xml: b"<P2P/>".to_vec(),
        })
        .encode()
        .unwrap();
        *datagram.last_mut().unwrap() ^= 1;

        assert!(matches!(
            BcUdpPacket::decode(&datagram),
            Err(BcError::UdpChecksumMismatch { .. })
        ));
    }

    #[test]
    fn transport_fragments_reorders_and_acknowledges_payload() {
        let now = Instant::now();
        let config = BcUdpConfig {
            mtu: 32,
            ..BcUdpConfig::default()
        };
        let mut sender = BcUdpTransport::new(10, 20, now, config.clone()).unwrap();
        let mut receiver = BcUdpTransport::new(20, 10, now, config).unwrap();
        let payload = (0..50).collect::<Vec<_>>();
        sender.queue_payload(&payload).unwrap();

        let mut datagrams = Vec::new();
        while let BcUdpOutput::Datagram(datagram) = sender.poll_output(now).unwrap() {
            datagrams.push(datagram);
        }
        assert_eq!(datagrams.len(), 5);

        for index in [1, 0, 3, 2, 4] {
            receiver.handle_datagram(&datagrams[index]).unwrap();
        }
        let mut assembled = Vec::new();
        while let BcUdpOutput::Payload(chunk) = receiver.poll_output(now).unwrap() {
            assembled.extend_from_slice(&chunk);
        }
        assert_eq!(assembled, payload);

        let BcUdpOutput::Datagram(ack) = receiver
            .poll_output(now + Duration::from_millis(10))
            .unwrap()
        else {
            panic!("expected ACK datagram");
        };
        sender.handle_datagram(&ack).unwrap();
        assert_eq!(sender.pending_send_packets(), 0);
    }

    #[test]
    fn transport_retransmits_unacknowledged_packets() {
        let now = Instant::now();
        let mut transport = BcUdpTransport::new(10, 20, now, BcUdpConfig::default()).unwrap();
        transport.queue_payload(b"retry me").unwrap();
        let BcUdpOutput::Datagram(first) = transport.poll_output(now).unwrap() else {
            panic!("expected initial datagram");
        };
        let _ = transport.poll_output(now).unwrap();

        let deadline = now + Duration::from_millis(500);
        let BcUdpOutput::Datagram(ack) = transport.poll_output(deadline).unwrap() else {
            panic!("expected periodic ACK datagram");
        };
        assert!(matches!(
            BcUdpPacket::decode(&ack).unwrap(),
            BcUdpPacket::Ack(_)
        ));
        let BcUdpOutput::Datagram(retry) = transport.poll_output(deadline).unwrap() else {
            panic!("expected retransmitted datagram");
        };
        assert_eq!(retry, first);
    }

    #[test]
    fn selective_ack_removes_only_received_packets() {
        let now = Instant::now();
        let config = BcUdpConfig {
            mtu: 21,
            ..BcUdpConfig::default()
        };
        let mut transport = BcUdpTransport::new(10, 20, now, config).unwrap();
        transport.queue_payload(b"abc").unwrap();
        assert_eq!(transport.pending_send_packets(), 3);
        let ack = BcUdpPacket::Ack(UdpAck {
            connection_id: 10,
            group_id: 0,
            packet_id: 0,
            latency: 0,
            received: vec![0, 1],
        })
        .encode()
        .unwrap();

        transport.handle_datagram(&ack).unwrap();
        assert_eq!(transport.pending_send_packets(), 1);
        assert!(transport.sent.contains_key(&1));
    }

    #[test]
    fn discovery_retries_and_negotiates_connection_ids() {
        let now = Instant::now();
        let config = BcUdpDiscoveryConfig {
            transmission_id: 87,
            ..BcUdpDiscoveryConfig::new("95270000YGAKNWKJ", -376_737_975, 53_612)
        };
        let mut discovery = BcUdpDiscovery::new(config, now).unwrap();
        let BcUdpDiscoveryOutput::Datagram(request) = discovery.poll_output(now) else {
            panic!("expected discovery request");
        };
        let BcUdpPacket::Discovery(request) = BcUdpPacket::decode(&request).unwrap() else {
            panic!("expected discovery packet");
        };
        let request_xml = String::from_utf8(request.xml).unwrap();
        assert!(request_xml.contains("<uid>95270000YGAKNWKJ</uid>"));
        assert!(request_xml.contains("<cli><port>53612</port></cli>"));
        assert_eq!(
            discovery.poll_output(now + Duration::from_millis(499)),
            BcUdpDiscoveryOutput::Timeout(now + Duration::from_millis(500)),
        );

        let reply = BcUdpPacket::Discovery(UdpDiscovery {
            transmission_id: 87,
            xml: b"<P2P><D2C_C_R><timer><def>3000</def><hb>20000</hb><hbt>60000</hbt></timer><rsp>0</rsp><cid>-376737975</cid><did>49</did></D2C_C_R></P2P>".to_vec(),
        })
        .encode()
        .unwrap();
        discovery.handle_datagram(&reply).unwrap();
        assert_eq!(
            discovery.poll_output(now + Duration::from_millis(499)),
            BcUdpDiscoveryOutput::Connected(BcUdpConnection {
                client_id: -376_737_975,
                camera_id: 49,
                transmission_id: 87,
            }),
        );
    }

    #[test]
    fn connection_builds_heartbeat_and_disconnect_packets() {
        let connection = BcUdpConnection {
            client_id: 82_000,
            camera_id: 80,
            transmission_id: 96,
        };
        for (datagram, element) in [
            (connection.heartbeat().unwrap(), "C2D_HB"),
            (connection.disconnect().unwrap(), "C2D_DISC"),
        ] {
            let BcUdpPacket::Discovery(discovery) = BcUdpPacket::decode(&datagram).unwrap() else {
                panic!("expected discovery packet");
            };
            let xml = String::from_utf8(discovery.xml).unwrap();
            assert!(xml.contains(&format!("<{element}>")));
            assert!(xml.contains("<cid>82000</cid>"));
            assert!(xml.contains("<did>80</did>"));
        }
    }
}
