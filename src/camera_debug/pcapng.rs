//! Pure-Rust bounded in-process PCAPNG writer with synthetic L2/L3/L4 headers.
//!
//! Writes Section Header, Interface Description (`LINKTYPE_ETHERNET`), and
//! Enhanced Packet blocks with microsecond timestamps and UTF-8 `opt_comment`
//! annotations so decrypted TLS and plaintext camera streams open directly in
//! Wireshark on Windows, macOS, and Linux without root or `libpcap`.

use anyhow::{Context, bail, ensure};
use std::{
    collections::HashMap,
    io::Write,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{SystemTime, UNIX_EPOCH},
};

const SHB_BLOCK_TYPE: u32 = 0x0A0D_0D0A;
const SHB_BYTE_ORDER_MAGIC: u32 = 0x1A2B_3C4D;
const IDB_BLOCK_TYPE: u32 = 0x0000_0001;
const EPB_BLOCK_TYPE: u32 = 0x0000_0006;

const LINKTYPE_ETHERNET: u16 = 1;
const IDB_SNAPLEN_BYTES: u32 = 262_144;

const OPT_END: u16 = 0;
const OPT_COMMENT: u16 = 1;
const OPT_IDB_NAME: u16 = 2;
const OPT_IDB_TSRESOL: u16 = 9;
const OPT_SHB_USERAPPL: u16 = 4;

const ETHERNET_HEADER_LEN: usize = 14;
const IPV4_HEADER_LEN: usize = 20;
const IPV6_HEADER_LEN: usize = 40;
const TCP_HEADER_LEN: usize = 20;
const UDP_HEADER_LEN: usize = 8;

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_IPV6: u16 = 0x86DD;
const IP_PROTO_TCP: u8 = 6;
const IP_PROTO_UDP: u8 = 17;

const TCP_FLAG_FIN: u16 = 0x0001;
const TCP_FLAG_SYN: u16 = 0x0002;
const TCP_FLAG_ACK: u16 = 0x0010;
const TCP_FLAG_PSH: u16 = 0x0008;

/// Maximum TCP segment payload per synthesized Ethernet frame to stay inside a 1500-byte MTU.
pub const MAX_TCP_SEGMENT_BYTES: usize = 1440;
/// Maximum UDP payload per synthesized IP datagram.
pub const MAX_UDP_PAYLOAD_BYTES: usize = 65_507;
/// Default maximum packet count accepted by one [`PcapngWriter`].
pub const DEFAULT_PCAPNG_MAX_PACKETS: u64 = 250_000;
/// Maximum length in bytes of one UTF-8 `opt_comment` annotation.
pub const MAX_COMMENT_BYTES: usize = 2048;

/// Direction of a captured socket flow relative to the local client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowDirection {
    /// Bytes sent from client to camera.
    ClientToServer,
    /// Bytes received from camera by client.
    ServerToClient,
}

/// Identifies a synthesized TCP connection for monotonic `seq`/`ack` tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TcpFlowKey {
    /// Local client socket endpoint.
    pub client: SocketAddr,
    /// Remote camera socket endpoint.
    pub server: SocketAddr,
}

#[derive(Debug, Clone, Copy)]
struct TcpFlowState {
    handshake_sent: bool,
    closed: bool,
    client_seq: u32,
    server_seq: u32,
}

/// Bounded in-process PCAPNG writer synthesizing Ethernet, IPv4/IPv6, TCP, and UDP frames.
pub struct PcapngWriter<W: Write> {
    writer: W,
    bytes_written: u64,
    max_bytes: u64,
    packet_count: u64,
    max_packets: u64,
    next_ip_id: u16,
    tcp_flows: HashMap<TcpFlowKey, TcpFlowState>,
}

impl<W: Write> PcapngWriter<W> {
    /// Creates a new PCAPNG writer and writes the initial Section Header and Interface blocks.
    ///
    /// # Errors
    /// Returns an error if `max_bytes` is smaller than the initial header blocks or writing fails.
    pub fn new(writer: W, max_bytes: u64) -> anyhow::Result<Self> {
        Self::with_limits(writer, max_bytes, DEFAULT_PCAPNG_MAX_PACKETS)
    }

    /// Creates a new PCAPNG writer with explicit byte and packet bounds.
    ///
    /// # Errors
    /// Returns an error if the limits are zero or smaller than the initial PCAPNG headers.
    pub fn with_limits(mut writer: W, max_bytes: u64, max_packets: u64) -> anyhow::Result<Self> {
        ensure!(max_bytes > 0, "PCAPNG max_bytes must be positive");
        ensure!(max_packets > 0, "PCAPNG max_packets must be positive");
        let shb = build_shb_block();
        let idb = build_idb_block();
        let header_bytes = u64::try_from(shb.len() + idb.len()).context("header length")?;
        ensure!(
            header_bytes <= max_bytes,
            "PCAPNG max_bytes ({max_bytes}) is smaller than required header ({header_bytes})"
        );
        writer.write_all(&shb)?;
        writer.write_all(&idb)?;
        Ok(Self {
            writer,
            bytes_written: header_bytes,
            max_bytes,
            packet_count: 0,
            max_packets,
            next_ip_id: 1,
            tcp_flows: HashMap::new(),
        })
    }

    /// Returns total PCAPNG bytes written so far, including headers.
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Returns total Enhanced Packet Blocks written so far.
    pub const fn packet_count(&self) -> u64 {
        self.packet_count
    }

    /// Consumes the writer and returns the flushed inner sink.
    ///
    /// # Errors
    /// Returns an error if flushing the underlying writer fails.
    pub fn finish(mut self) -> anyhow::Result<W> {
        self.writer.flush()?;
        Ok(self.writer)
    }

    /// Flushes buffered bytes to the underlying sink.
    ///
    /// # Errors
    /// Returns an error if the underlying writer flush fails.
    pub fn flush(&mut self) -> anyhow::Result<()> {
        self.writer.flush()?;
        Ok(())
    }

    /// Writes a synthetic TCP payload (emitting `SYN`/`SYN-ACK`/`ACK` on first write) as EPBs.
    ///
    /// Returns `Ok(true)` when all segments fit within the writer bounds, or `Ok(false)` when
    /// the byte or packet cap prevented writing further packets.
    ///
    /// # Errors
    /// Returns an error on address-family mismatch, oversized comment, or I/O failure.
    pub fn write_tcp_payload(
        &mut self,
        flow: TcpFlowKey,
        direction: FlowDirection,
        timestamp_us: u64,
        payload: &[u8],
        comment: Option<&str>,
    ) -> anyhow::Result<bool> {
        ensure!(
            flow.client.is_ipv4() == flow.server.is_ipv4(),
            "TCP endpoints must share IPv4 or IPv6 address family"
        );
        if !self.ensure_tcp_handshake(flow, timestamp_us, comment)? {
            return Ok(false);
        }
        if payload.is_empty() {
            return Ok(true);
        }
        for chunk in payload.chunks(MAX_TCP_SEGMENT_BYTES) {
            if !self.write_tcp_segment(flow, direction, timestamp_us, chunk, comment)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Writes a synthetic TCP `FIN|ACK` and `ACK` close sequence for an open flow.
    ///
    /// # Errors
    /// Returns an error on address-family mismatch or I/O failure.
    pub fn close_tcp_flow(
        &mut self,
        flow: TcpFlowKey,
        direction: FlowDirection,
        timestamp_us: u64,
        comment: Option<&str>,
    ) -> anyhow::Result<bool> {
        let Some(state) = self.tcp_flows.get_mut(&flow) else {
            return Ok(true);
        };
        if !state.handshake_sent || state.closed {
            return Ok(true);
        }
        let (src, dst, seq, ack) = match direction {
            FlowDirection::ClientToServer => {
                let seq = state.client_seq;
                state.client_seq = state.client_seq.wrapping_add(1);
                (flow.client, flow.server, seq, state.server_seq)
            }
            FlowDirection::ServerToClient => {
                let seq = state.server_seq;
                state.server_seq = state.server_seq.wrapping_add(1);
                (flow.server, flow.client, seq, state.client_seq)
            }
        };
        state.closed = true;
        let frame = self.build_tcp_frame(src, dst, seq, ack, TCP_FLAG_FIN | TCP_FLAG_ACK, &[])?;
        self.write_epb(timestamp_us, &frame, comment)
    }

    /// Writes a synthetic UDP datagram as an Enhanced Packet Block.
    ///
    /// Returns `Ok(true)` if written, or `Ok(false)` if writer byte/packet caps were reached.
    ///
    /// # Errors
    /// Returns an error if the endpoints differ in IP family, payload exceeds `65_507` bytes,
    /// comment exceeds bounds, or writing fails.
    pub fn write_udp_packet(
        &mut self,
        src: SocketAddr,
        dst: SocketAddr,
        timestamp_us: u64,
        payload: &[u8],
        comment: Option<&str>,
    ) -> anyhow::Result<bool> {
        ensure!(
            src.is_ipv4() == dst.is_ipv4(),
            "UDP endpoints must share IPv4 or IPv6 address family"
        );
        ensure!(
            payload.len() <= MAX_UDP_PAYLOAD_BYTES,
            "UDP payload length {} exceeds maximum {MAX_UDP_PAYLOAD_BYTES}",
            payload.len()
        );
        let ip_id = self.alloc_ip_id();
        let frame = build_udp_frame(src, dst, ip_id, payload)?;
        self.write_epb(timestamp_us, &frame, comment)
    }

    fn ensure_tcp_handshake(
        &mut self,
        flow: TcpFlowKey,
        timestamp_us: u64,
        comment: Option<&str>,
    ) -> anyhow::Result<bool> {
        let state = *self.tcp_flows.entry(flow).or_insert(TcpFlowState {
            handshake_sent: false,
            closed: false,
            client_seq: 1_000,
            server_seq: 2_000,
        });
        if state.handshake_sent {
            return Ok(true);
        }
        let client_isn = state.client_seq;
        let server_isn = state.server_seq;
        let syn =
            self.build_tcp_frame(flow.client, flow.server, client_isn, 0, TCP_FLAG_SYN, &[])?;
        if !self.write_epb(timestamp_us, &syn, comment)? {
            return Ok(false);
        }
        let syn_ack = self.build_tcp_frame(
            flow.server,
            flow.client,
            server_isn,
            client_isn.wrapping_add(1),
            TCP_FLAG_SYN | TCP_FLAG_ACK,
            &[],
        )?;
        if !self.write_epb(timestamp_us, &syn_ack, comment)? {
            return Ok(false);
        }
        let ack = self.build_tcp_frame(
            flow.client,
            flow.server,
            client_isn.wrapping_add(1),
            server_isn.wrapping_add(1),
            TCP_FLAG_ACK,
            &[],
        )?;
        if !self.write_epb(timestamp_us, &ack, comment)? {
            return Ok(false);
        }
        let entry = self
            .tcp_flows
            .get_mut(&flow)
            .expect("TCP flow entry must exist after insertion");
        entry.handshake_sent = true;
        entry.client_seq = client_isn.wrapping_add(1);
        entry.server_seq = server_isn.wrapping_add(1);
        Ok(true)
    }

    fn write_tcp_segment(
        &mut self,
        flow: TcpFlowKey,
        direction: FlowDirection,
        timestamp_us: u64,
        chunk: &[u8],
        comment: Option<&str>,
    ) -> anyhow::Result<bool> {
        let state = *self
            .tcp_flows
            .get(&flow)
            .expect("TCP flow must be initialized before writing segments");
        let (src, dst, seq, ack) = match direction {
            FlowDirection::ClientToServer => {
                (flow.client, flow.server, state.client_seq, state.server_seq)
            }
            FlowDirection::ServerToClient => {
                (flow.server, flow.client, state.server_seq, state.client_seq)
            }
        };
        let frame = self.build_tcp_frame(src, dst, seq, ack, TCP_FLAG_PSH | TCP_FLAG_ACK, chunk)?;
        if !self.write_epb(timestamp_us, &frame, comment)? {
            return Ok(false);
        }
        let advance = u32::try_from(chunk.len()).context("TCP segment length")?;
        let entry = self
            .tcp_flows
            .get_mut(&flow)
            .expect("TCP flow must remain present");
        match direction {
            FlowDirection::ClientToServer => entry.client_seq = seq.wrapping_add(advance),
            FlowDirection::ServerToClient => entry.server_seq = seq.wrapping_add(advance),
        }
        Ok(true)
    }

    fn build_tcp_frame(
        &mut self,
        src: SocketAddr,
        dst: SocketAddr,
        seq: u32,
        ack: u32,
        flags: u16,
        payload: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        let ip_id = self.alloc_ip_id();
        build_tcp_frame_with_id(src, dst, ip_id, seq, ack, flags, payload)
    }

    const fn alloc_ip_id(&mut self) -> u16 {
        let id = self.next_ip_id;
        self.next_ip_id = self.next_ip_id.wrapping_add(1);
        if self.next_ip_id == 0 {
            self.next_ip_id = 1;
        }
        id
    }

    fn write_epb(
        &mut self,
        timestamp_us: u64,
        frame: &[u8],
        comment: Option<&str>,
    ) -> anyhow::Result<bool> {
        if self.packet_count >= self.max_packets {
            return Ok(false);
        }
        if let Some(text) = comment {
            ensure!(
                text.len() <= MAX_COMMENT_BYTES,
                "PCAPNG opt_comment length {} exceeds {MAX_COMMENT_BYTES}",
                text.len()
            );
        }
        let block = build_epb_block(timestamp_us, frame, comment)?;
        let block_len = u64::try_from(block.len()).context("EPB length")?;
        if self.bytes_written.saturating_add(block_len) > self.max_bytes {
            return Ok(false);
        }
        self.writer.write_all(&block)?;
        self.bytes_written += block_len;
        self.packet_count += 1;
        Ok(true)
    }
}

/// Returns current wall-clock microseconds since the Unix epoch.
#[must_use]
pub fn unix_timestamp_us() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Formats a structured UTF-8 PCAPNG packet comment for decrypted or plaintext camera traffic.
#[must_use]
pub fn format_packet_comment(tls_decrypted: bool, camera: &str, proto: &str) -> String {
    let sanitized_camera: String = camera
        .chars()
        .filter(|ch| !ch.is_control() && *ch != ';')
        .take(96)
        .collect();
    let sanitized_proto: String = proto
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .take(32)
        .collect();
    format!("tls_decrypted={tls_decrypted};camera={sanitized_camera};proto={sanitized_proto}")
}

fn build_shb_block() -> Vec<u8> {
    let mut body = Vec::with_capacity(64);
    body.extend_from_slice(&SHB_BYTE_ORDER_MAGIC.to_le_bytes());
    body.extend_from_slice(&1_u16.to_le_bytes());
    body.extend_from_slice(&0_u16.to_le_bytes());
    body.extend_from_slice(&u64::MAX.to_le_bytes());
    append_option(&mut body, OPT_SHB_USERAPPL, b"keeppeek-camera-debug");
    append_option(&mut body, OPT_END, &[]);
    wrap_pcapng_block(SHB_BLOCK_TYPE, &body)
}

fn build_idb_block() -> Vec<u8> {
    let mut body = Vec::with_capacity(64);
    body.extend_from_slice(&LINKTYPE_ETHERNET.to_le_bytes());
    body.extend_from_slice(&0_u16.to_le_bytes());
    body.extend_from_slice(&IDB_SNAPLEN_BYTES.to_le_bytes());
    append_option(&mut body, OPT_IDB_NAME, b"keeppeek0");
    append_option(&mut body, OPT_IDB_TSRESOL, &[6]);
    append_option(&mut body, OPT_END, &[]);
    wrap_pcapng_block(IDB_BLOCK_TYPE, &body)
}

fn build_epb_block(
    timestamp_us: u64,
    frame: &[u8],
    comment: Option<&str>,
) -> anyhow::Result<Vec<u8>> {
    let frame_len = u32::try_from(frame.len()).context("frame length exceeds u32")?;
    let ts_high = (timestamp_us >> 32) as u32;
    let ts_low = (timestamp_us & 0xFFFF_FFFF) as u32;
    let mut body = Vec::with_capacity(32 + frame.len() + 64);
    body.extend_from_slice(&0_u32.to_le_bytes());
    body.extend_from_slice(&ts_high.to_le_bytes());
    body.extend_from_slice(&ts_low.to_le_bytes());
    body.extend_from_slice(&frame_len.to_le_bytes());
    body.extend_from_slice(&frame_len.to_le_bytes());
    body.extend_from_slice(frame);
    pad_to_4_bytes(&mut body);
    if let Some(comment) = comment
        && !comment.is_empty()
    {
        append_option(&mut body, OPT_COMMENT, comment.as_bytes());
        append_option(&mut body, OPT_END, &[]);
    }
    Ok(wrap_pcapng_block(EPB_BLOCK_TYPE, &body))
}

fn append_option(buf: &mut Vec<u8>, code: u16, value: &[u8]) {
    let len = u16::try_from(value.len()).expect("PCAPNG option length must fit in u16");
    buf.extend_from_slice(&code.to_le_bytes());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(value);
    pad_to_4_bytes(buf);
}

fn pad_to_4_bytes(buf: &mut Vec<u8>) {
    let rem = buf.len() % 4;
    if rem != 0 {
        buf.resize(buf.len() + (4 - rem), 0);
    }
}

fn wrap_pcapng_block(block_type: u32, body: &[u8]) -> Vec<u8> {
    assert_eq!(
        body.len() % 4,
        0,
        "PCAPNG block body must be 32-bit aligned"
    );
    let total_len = u32::try_from(12 + body.len()).expect("PCAPNG block length must fit in u32");
    let mut block = Vec::with_capacity(total_len as usize);
    block.extend_from_slice(&block_type.to_le_bytes());
    block.extend_from_slice(&total_len.to_le_bytes());
    block.extend_from_slice(body);
    block.extend_from_slice(&total_len.to_le_bytes());
    block
}

fn build_tcp_frame_with_id(
    src: SocketAddr,
    dst: SocketAddr,
    ip_id: u16,
    seq: u32,
    ack: u32,
    flags: u16,
    payload: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let l4_len = TCP_HEADER_LEN + payload.len();
    let mut tcp = Vec::with_capacity(l4_len);
    tcp.extend_from_slice(&src.port().to_be_bytes());
    tcp.extend_from_slice(&dst.port().to_be_bytes());
    tcp.extend_from_slice(&seq.to_be_bytes());
    tcp.extend_from_slice(&ack.to_be_bytes());
    let data_offset_and_flags = (5_u16 << 12) | (flags & 0x0FFF);
    tcp.extend_from_slice(&data_offset_and_flags.to_be_bytes());
    tcp.extend_from_slice(&65_535_u16.to_be_bytes());
    tcp.extend_from_slice(&0_u16.to_be_bytes());
    tcp.extend_from_slice(&0_u16.to_be_bytes());
    tcp.extend_from_slice(payload);
    let checksum = transport_checksum(src.ip(), dst.ip(), IP_PROTO_TCP, &tcp)?;
    tcp[16..18].copy_from_slice(&checksum.to_be_bytes());
    wrap_ip_and_ethernet(src.ip(), dst.ip(), ip_id, IP_PROTO_TCP, &tcp)
}

fn build_udp_frame(
    src: SocketAddr,
    dst: SocketAddr,
    ip_id: u16,
    payload: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let udp_len = u16::try_from(UDP_HEADER_LEN + payload.len()).context("UDP length")?;
    let mut udp = Vec::with_capacity(udp_len as usize);
    udp.extend_from_slice(&src.port().to_be_bytes());
    udp.extend_from_slice(&dst.port().to_be_bytes());
    udp.extend_from_slice(&udp_len.to_be_bytes());
    udp.extend_from_slice(&0_u16.to_be_bytes());
    udp.extend_from_slice(payload);
    let mut checksum = transport_checksum(src.ip(), dst.ip(), IP_PROTO_UDP, &udp)?;
    if checksum == 0 {
        checksum = 0xFFFF;
    }
    udp[6..8].copy_from_slice(&checksum.to_be_bytes());
    wrap_ip_and_ethernet(src.ip(), dst.ip(), ip_id, IP_PROTO_UDP, &udp)
}

fn wrap_ip_and_ethernet(
    src: IpAddr,
    dst: IpAddr,
    ip_id: u16,
    proto: u8,
    l4_bytes: &[u8],
) -> anyhow::Result<Vec<u8>> {
    match (src, dst) {
        (IpAddr::V4(s), IpAddr::V4(d)) => {
            let total_len =
                u16::try_from(IPV4_HEADER_LEN + l4_bytes.len()).context("IPv4 packet too large")?;
            let mut frame = Vec::with_capacity(ETHERNET_HEADER_LEN + total_len as usize);
            append_ethernet_header(&mut frame, src, dst, ETHERTYPE_IPV4);
            let ip_start = frame.len();
            append_ipv4_header(&mut frame, s, d, ip_id, proto, total_len);
            let csum = internet_checksum(&frame[ip_start..ip_start + IPV4_HEADER_LEN]);
            frame[ip_start + 10..ip_start + 12].copy_from_slice(&csum.to_be_bytes());
            frame.extend_from_slice(l4_bytes);
            Ok(frame)
        }
        (IpAddr::V6(s), IpAddr::V6(d)) => {
            let payload_len =
                u16::try_from(l4_bytes.len()).context("IPv6 payload exceeds u16::MAX")?;
            let mut frame =
                Vec::with_capacity(ETHERNET_HEADER_LEN + IPV6_HEADER_LEN + l4_bytes.len());
            append_ethernet_header(&mut frame, src, dst, ETHERTYPE_IPV6);
            append_ipv6_header(&mut frame, s, d, proto, payload_len);
            frame.extend_from_slice(l4_bytes);
            Ok(frame)
        }
        _ => bail!("mismatched IPv4/IPv6 addresses"),
    }
}

fn append_ethernet_header(frame: &mut Vec<u8>, src: IpAddr, dst: IpAddr, ethertype: u16) {
    frame.extend_from_slice(&synthetic_mac(dst));
    frame.extend_from_slice(&synthetic_mac(src));
    frame.extend_from_slice(&ethertype.to_be_bytes());
}

const fn synthetic_mac(ip: IpAddr) -> [u8; 6] {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            [0x02, 0x00, o[0], o[1], o[2], o[3]]
        }
        IpAddr::V6(v6) => {
            let o = v6.octets();
            [0x02, 0x00, o[12], o[13], o[14], o[15]]
        }
    }
}

fn append_ipv4_header(
    buf: &mut Vec<u8>,
    src: Ipv4Addr,
    dst: Ipv4Addr,
    ip_id: u16,
    proto: u8,
    total_len: u16,
) {
    buf.push(0x45);
    buf.push(0x00);
    buf.extend_from_slice(&total_len.to_be_bytes());
    buf.extend_from_slice(&ip_id.to_be_bytes());
    buf.extend_from_slice(&0x4000_u16.to_be_bytes());
    buf.push(64);
    buf.push(proto);
    buf.extend_from_slice(&0_u16.to_be_bytes());
    buf.extend_from_slice(&src.octets());
    buf.extend_from_slice(&dst.octets());
}

fn append_ipv6_header(
    buf: &mut Vec<u8>,
    src: Ipv6Addr,
    dst: Ipv6Addr,
    next_header: u8,
    payload_len: u16,
) {
    buf.extend_from_slice(&0x6000_0000_u32.to_be_bytes());
    buf.extend_from_slice(&payload_len.to_be_bytes());
    buf.push(next_header);
    buf.push(64);
    buf.extend_from_slice(&src.octets());
    buf.extend_from_slice(&dst.octets());
}

fn transport_checksum(src: IpAddr, dst: IpAddr, proto: u8, segment: &[u8]) -> anyhow::Result<u16> {
    let mut sum = 0_u32;
    match (src, dst) {
        (IpAddr::V4(s), IpAddr::V4(d)) => {
            sum = sum_be_words(&s.octets(), sum);
            sum = sum_be_words(&d.octets(), sum);
            sum += u32::from(proto);
            sum += u32::try_from(segment.len()).context("IPv4 L4 length")?;
        }
        (IpAddr::V6(s), IpAddr::V6(d)) => {
            sum = sum_be_words(&s.octets(), sum);
            sum = sum_be_words(&d.octets(), sum);
            let len = u32::try_from(segment.len()).context("IPv6 L4 length")?;
            sum += (len >> 16) + (len & 0xFFFF);
            sum += u32::from(proto);
        }
        _ => bail!("mismatched IPv4/IPv6 addresses for checksum"),
    }
    sum = sum_be_words(segment, sum);
    Ok(fold_ones_complement(sum))
}

/// Computes the RFC 1071 one's-complement Internet checksum over `bytes`.
#[must_use]
pub fn internet_checksum(bytes: &[u8]) -> u16 {
    fold_ones_complement(sum_be_words(bytes, 0))
}

fn sum_be_words(bytes: &[u8], mut sum: u32) -> u32 {
    let (chunks, remainder) = bytes.as_chunks::<2>();
    for &pair in chunks {
        sum += u32::from(u16::from_be_bytes(pair));
    }
    if let [last] = remainder {
        sum += u32::from(u16::from_be_bytes([*last, 0]));
    }
    sum
}

const fn fold_ones_complement(mut sum: u32) -> u16 {
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ParsedBlock {
        block_type: u32,
        body: Vec<u8>,
    }

    fn parse_blocks(bytes: &[u8]) -> Vec<ParsedBlock> {
        let mut pos = 0;
        let mut blocks = Vec::new();
        while pos < bytes.len() {
            let block_type = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
            let total_len =
                u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
            assert_eq!(total_len % 4, 0);
            let trailing = u32::from_le_bytes(
                bytes[pos + total_len - 4..pos + total_len]
                    .try_into()
                    .unwrap(),
            ) as usize;
            assert_eq!(total_len, trailing);
            blocks.push(ParsedBlock {
                block_type,
                body: bytes[pos + 8..pos + total_len - 4].to_vec(),
            });
            pos += total_len;
        }
        blocks
    }

    fn extract_epb_frame(body: &[u8]) -> (u64, &[u8], Option<String>) {
        let ts_high = u64::from(u32::from_le_bytes(body[4..8].try_into().unwrap()));
        let ts_low = u64::from(u32::from_le_bytes(body[8..12].try_into().unwrap()));
        let cap_len = u32::from_le_bytes(body[12..16].try_into().unwrap()) as usize;
        let orig_len = u32::from_le_bytes(body[16..20].try_into().unwrap()) as usize;
        assert_eq!(cap_len, orig_len);
        let frame = &body[20..20 + cap_len];
        let padded = 20 + ((cap_len + 3) & !3);
        let mut comment = None;
        let mut opt_pos = padded;
        while opt_pos + 4 <= body.len() {
            let code = u16::from_le_bytes(body[opt_pos..opt_pos + 2].try_into().unwrap());
            let len =
                u16::from_le_bytes(body[opt_pos + 2..opt_pos + 4].try_into().unwrap()) as usize;
            opt_pos += 4;
            if code == OPT_END {
                break;
            }
            if code == OPT_COMMENT {
                comment = Some(
                    std::str::from_utf8(&body[opt_pos..opt_pos + len])
                        .unwrap()
                        .to_owned(),
                );
            }
            opt_pos += (len + 3) & !3;
        }
        ((ts_high << 32) | ts_low, frame, comment)
    }

    #[test]
    fn writes_valid_shb_idb_and_tcp_handshake_with_checksums() {
        let mut writer = PcapngWriter::new(Vec::new(), 1024 * 1024).unwrap();
        let flow = TcpFlowKey {
            client: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20)), 51000),
            server: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 554),
        };
        let comment = format_packet_comment(true, "front-door", "rtsps");
        assert!(
            writer
                .write_tcp_payload(
                    flow,
                    FlowDirection::ClientToServer,
                    1_700_000_000_123_456,
                    b"DESCRIBE rtsp://192.0.2.10/stream1 RTSP/1.0\r\n\r\n",
                    Some(&comment),
                )
                .unwrap()
        );
        assert!(
            writer
                .write_tcp_payload(
                    flow,
                    FlowDirection::ServerToClient,
                    1_700_000_000_200_000,
                    b"RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\n",
                    Some(&comment),
                )
                .unwrap()
        );
        assert!(
            writer
                .close_tcp_flow(
                    flow,
                    FlowDirection::ClientToServer,
                    1_700_000_000_300_000,
                    None
                )
                .unwrap()
        );
        let bytes = writer.finish().unwrap();
        let blocks = parse_blocks(&bytes);
        assert_eq!(blocks.len(), 8);
        assert_eq!(blocks[0].block_type, SHB_BLOCK_TYPE);
        assert_eq!(
            u32::from_le_bytes(blocks[0].body[0..4].try_into().unwrap()),
            SHB_BYTE_ORDER_MAGIC
        );
        assert_eq!(blocks[1].block_type, IDB_BLOCK_TYPE);
        assert_eq!(
            u16::from_le_bytes(blocks[1].body[0..2].try_into().unwrap()),
            LINKTYPE_ETHERNET
        );
        verify_tcp_epb_checksums(&blocks[2..=6], &comment);
        let (ts, _frame, fin_comment) = extract_epb_frame(&blocks[7].body);
        assert_eq!(ts, 1_700_000_000_300_000);
        assert!(fin_comment.is_none());
    }

    fn verify_tcp_epb_checksums(blocks: &[ParsedBlock], expected_comment: &str) {
        let mut prev_client_seq = None;
        for block in blocks {
            assert_eq!(block.block_type, EPB_BLOCK_TYPE);
            let (_ts, frame, parsed_comment) = extract_epb_frame(&block.body);
            assert_eq!(parsed_comment.as_deref(), Some(expected_comment));
            assert_eq!(u16::from_be_bytes([frame[12], frame[13]]), ETHERTYPE_IPV4);
            let ip = &frame[ETHERNET_HEADER_LEN..];
            assert_eq!(internet_checksum(&ip[..IPV4_HEADER_LEN]), 0);
            let tcp = &ip[IPV4_HEADER_LEN..];
            let src_ip = IpAddr::V4(Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]));
            let dst_ip = IpAddr::V4(Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]));
            assert_eq!(
                transport_checksum(src_ip, dst_ip, IP_PROTO_TCP, tcp).unwrap(),
                0
            );
            if u16::from_be_bytes([tcp[2], tcp[3]]) == 554 {
                let seq = u32::from_be_bytes(tcp[4..8].try_into().unwrap());
                if let Some(prev) = prev_client_seq {
                    assert!(seq >= prev);
                }
                prev_client_seq = Some(seq);
            }
        }
    }

    #[test]
    fn writes_valid_ipv6_udp_and_enforces_bounds() {
        let mut writer = PcapngWriter::new(Vec::new(), 4096).unwrap();
        let src = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 40000);
        let dst = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 5004);
        assert!(
            writer
                .write_udp_packet(src, dst, 42, b"\x80\x60\x00\x01payload", Some("proto=rtp"))
                .unwrap()
        );
        let bytes = writer.finish().unwrap();
        let blocks = parse_blocks(&bytes);
        assert_eq!(blocks.len(), 3);
        let (ts, frame, comment) = extract_epb_frame(&blocks[2].body);
        assert_eq!(ts, 42);
        assert_eq!(comment.as_deref(), Some("proto=rtp"));
        assert_eq!(u16::from_be_bytes([frame[12], frame[13]]), ETHERTYPE_IPV6);
        let udp = &frame[ETHERNET_HEADER_LEN + IPV6_HEADER_LEN..];
        assert_eq!(
            transport_checksum(src.ip(), dst.ip(), IP_PROTO_UDP, udp).unwrap(),
            0
        );

        let mut tiny = PcapngWriter::new(Vec::new(), 128).unwrap();
        assert!(
            !tiny
                .write_udp_packet(src, dst, 1, &[0_u8; 200], None)
                .unwrap()
        );
        assert_eq!(tiny.packet_count(), 0);
    }

    #[test]
    fn splits_large_tcp_payload_and_rejects_mismatched_families() {
        let mut writer = PcapngWriter::new(Vec::new(), 1024 * 1024).unwrap();
        let v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234);
        let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 554);
        assert!(
            writer
                .write_tcp_payload(
                    TcpFlowKey {
                        client: v4,
                        server: v6
                    },
                    FlowDirection::ClientToServer,
                    1,
                    b"hi",
                    None,
                )
                .is_err()
        );

        let flow = TcpFlowKey {
            client: v4,
            server: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 554),
        };
        let payload = vec![0xAB_u8; MAX_TCP_SEGMENT_BYTES * 2 + 100];
        assert!(
            writer
                .write_tcp_payload(flow, FlowDirection::ServerToClient, 99, &payload, None)
                .unwrap()
        );
        assert_eq!(writer.packet_count(), 6);
    }
}
