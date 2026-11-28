use super::{
    BrokerFailure, BrokerFailureKind,
    config::MqttForwarderConfig,
    model::{Publication, status_topic},
};
use mqtt_protocol_core::mqtt::{
    self,
    common::Cursor,
    connection::{ConnectionOptions, Event, TimerKind},
    packet::{
        ContentType, CorrelationData, MaximumPacketSize, Packet, PayloadFormat,
        PayloadFormatIndicator, Property, Qos, SessionExpiryInterval, UserProperty, v5_0,
    },
    prelude::GenericPacketTrait as _,
    result_code::{
        ConnectReasonCode, DisconnectReasonCode, MqttError, PubackReasonCode, PubcompReasonCode,
        PubrecReasonCode,
    },
};
use native_tls::{Certificate, HandshakeError, TlsConnector, TlsStream};
use std::{
    collections::VecDeque,
    fmt,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};
use url::{Host, Url};

/// Keep-alive interval in seconds advertised in MQTT 5 CONNECT packets.
///
/// Fifteen seconds detects idle broker drops quickly without adding noisy traffic.
const MQTT_KEEP_ALIVE_SECS: u16 = 15;

/// Maximum wait in milliseconds for a broker PINGRESP after sending PINGREQ.
///
/// Matches the 10-second forwarder operation timeout in `runtime.rs`.
const MQTT_PINGRESP_TIMEOUT_MS: u64 = 10_000;

const _: () = assert!(
    MQTT_PINGRESP_TIMEOUT_MS < (MQTT_KEEP_ALIVE_SECS as u64) * 1_000,
    "PINGRESP timeout must stay below the keep-alive interval"
);

/// Maximum MQTT 5 packet size in bytes accepted on receive and advertised in CONNECT.
///
/// Sixteen MiB matches the previous forwarder ceiling and bounds `PacketBuilder` allocations.
const MQTT_MAX_PACKET_BYTES: u32 = 16 * 1_024 * 1_024;

/// MQTT 5 session expiry interval in seconds requested during CONNECT.
///
/// Twenty-four hours lets a reconnecting broker resume session state after brief outages.
const MQTT_SESSION_EXPIRY_SECS: u32 = 24 * 60 * 60;

/// Reusable socket read buffer size in bytes for MQTT control and ack frames.
const READ_BUFFER_BYTES: usize = 8 * 1_024;

/// Maximum resolved socket addresses tried during one TCP connect attempt.
const CONNECT_ADDRESS_COUNT_MAX: usize = 32;

/// Maximum PEM certificate blocks loaded from one custom CA bundle file.
const CA_CERTIFICATE_COUNT_MAX: usize = 64;

/// Maximum `WouldBlock` handshake iterations allowed before failing TLS negotiation.
const TLS_HANDSHAKE_STEP_COUNT_MAX: usize = 64;

/// Minimum per-address TCP connect slice when multiple addresses resolve for one host.
const CONNECT_SLICE_MS_MIN: u64 = 250;

/// Maximum write wait in milliseconds for best-effort DISCONNECT on keep-alive timeout.
const KEEP_ALIVE_DISCONNECT_TIMEOUT_MS: u64 = 250;

const PEM_CERT_BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const PEM_CERT_END: &str = "-----END CERTIFICATE-----";

struct BrokerOptions {
    host: String,
    port: u16,
    use_tls: bool,
    tls_ca_pem: Option<Vec<u8>>,
    client_id: String,
    forwarder_id: String,
    username: Option<String>,
    password: Option<String>,
    will_topic: String,
    will_payload: Vec<u8>,
    will_qos: Qos,
    will_retain: bool,
    keep_alive_secs: u16,
    pingreq_interval_override_ms: Option<u64>,
    pingresp_timeout_ms: u64,
}

impl fmt::Debug for BrokerOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrokerOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("use_tls", &self.use_tls)
            .field("tls_ca_configured", &self.tls_ca_pem.is_some())
            .field("client_id", &self.client_id)
            .field("forwarder_id", &self.forwarder_id)
            .field("username_configured", &self.username.is_some())
            .field("password_configured", &self.password.is_some())
            .field("will_qos", &self.will_qos)
            .field("will_retain", &self.will_retain)
            .field("keep_alive_secs", &self.keep_alive_secs)
            .finish()
    }
}

impl BrokerOptions {
    fn from_config(config: &MqttForwarderConfig) -> Result<Self, BrokerFailure> {
        config.validate().map_err(|error| BrokerFailure {
            kind: BrokerFailureKind::Protocol,
            detail: error.to_string(),
        })?;
        let url = Url::parse(&config.broker_url).map_err(|_| BrokerFailure {
            kind: BrokerFailureKind::Protocol,
            detail: "MQTT broker URL is invalid.".to_owned(),
        })?;
        let host = match url.host() {
            Some(Host::Domain(domain)) => domain.to_owned(),
            Some(Host::Ipv4(address)) => address.to_string(),
            Some(Host::Ipv6(address)) => address.to_string(),
            None => {
                return Err(BrokerFailure {
                    kind: BrokerFailureKind::Protocol,
                    detail: "MQTT broker URL has no host.".to_owned(),
                });
            }
        };
        let use_tls = url.scheme() == "mqtts";
        let default_port = if use_tls { 8883 } else { 1883 };
        let port = url.port().unwrap_or(default_port);
        let tls_ca_pem = if use_tls {
            match &config.tls_ca_path {
                Some(path) => Some(std::fs::read(path).map_err(|_| BrokerFailure {
                    kind: BrokerFailureKind::Tls,
                    detail: "Configured MQTT CA certificate could not be read.".to_owned(),
                })?),
                None => None,
            }
        } else {
            None
        };
        let will_payload = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "state": "disconnected",
            "forwarder_id": config.forwarder_id,
        }))
        .map_err(|_| BrokerFailure {
            kind: BrokerFailureKind::Protocol,
            detail: "MQTT Last Will status could not be encoded.".to_owned(),
        })?;
        Ok(Self {
            host,
            port,
            use_tls,
            tls_ca_pem,
            client_id: config.client_id.clone(),
            forwarder_id: config.forwarder_id.clone(),
            username: config.username.clone(),
            password: config.password.clone(),
            will_topic: status_topic(config),
            will_payload,
            will_qos: qos(config.qos),
            will_retain: config.retain_health,
            keep_alive_secs: MQTT_KEEP_ALIVE_SECS,
            pingreq_interval_override_ms: None,
            pingresp_timeout_ms: MQTT_PINGRESP_TIMEOUT_MS,
        })
    }

    fn build_connect_packet(&self) -> Result<v5_0::Connect, BrokerFailure> {
        let props = vec![
            Property::SessionExpiryInterval(
                SessionExpiryInterval::new(MQTT_SESSION_EXPIRY_SECS)
                    .map_err(|_| protocol_settings_failure())?,
            ),
            Property::MaximumPacketSize(
                MaximumPacketSize::new(MQTT_MAX_PACKET_BYTES)
                    .map_err(|_| protocol_settings_failure())?,
            ),
        ];
        let will_props = vec![
            Property::PayloadFormatIndicator(
                PayloadFormatIndicator::new(PayloadFormat::String)
                    .map_err(|_| protocol_settings_failure())?,
            ),
            Property::ContentType(
                ContentType::new("application/json").map_err(|_| protocol_settings_failure())?,
            ),
            Property::CorrelationData(
                CorrelationData::new(self.forwarder_id.as_str())
                    .map_err(|_| protocol_settings_failure())?,
            ),
            Property::UserProperty(
                UserProperty::new("schema-version", "1")
                    .map_err(|_| protocol_settings_failure())?,
            ),
        ];

        let mut builder = v5_0::Connect::builder()
            .client_id(self.client_id.as_str())
            .map_err(|_| protocol_settings_failure())?
            .clean_start(false)
            .keep_alive(self.keep_alive_secs)
            .props(props)
            .will_message(
                self.will_topic.as_str(),
                &self.will_payload,
                self.will_qos,
                self.will_retain,
            )
            .map_err(|_| protocol_settings_failure())?
            .will_props(will_props);
        if let Some(username) = &self.username {
            builder = builder
                .user_name(username.as_str())
                .map_err(|_| protocol_settings_failure())?;
            if let Some(password) = &self.password {
                builder = builder
                    .password(password.as_str())
                    .map_err(|_| protocol_settings_failure())?;
            }
        }
        builder.build().map_err(|_| protocol_settings_failure())
    }
}

enum BrokerTransport {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl BrokerTransport {
    fn connect(options: &BrokerOptions, deadline: Instant) -> Result<Self, BrokerFailure> {
        let tcp = connect_tcp(&options.host, options.port, deadline)?;
        if !options.use_tls {
            return Ok(Self::Plain(tcp));
        }
        let connector = build_tls_connector(options.tls_ca_pem.as_deref())?;
        let tls = finish_tls_handshake(&connector, &options.host, tcp, deadline)?;
        Ok(Self::Tls(Box::new(tls)))
    }

    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_read_timeout(timeout),
            Self::Tls(stream) => stream.get_mut().set_read_timeout(timeout),
        }
    }

    fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_write_timeout(timeout),
            Self::Tls(stream) => stream.get_mut().set_write_timeout(timeout),
        }
    }

    fn shutdown(&mut self) {
        match self {
            Self::Plain(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
            Self::Tls(stream) => {
                let _ = stream.shutdown();
                let _ = stream.get_ref().shutdown(Shutdown::Both);
            }
        }
    }
}

impl Read for BrokerTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for BrokerTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

struct ActiveSession {
    transport: BrokerTransport,
    connection: mqtt::Connection<mqtt::role::Client>,
    pingresp_timeout_ms: u64,
    pingreq_send_at: Option<Instant>,
    pingresp_recv_at: Option<Instant>,
    pending_packets: VecDeque<Packet>,
    read_buffer: Vec<u8>,
}

impl ActiveSession {
    fn open(options: &BrokerOptions, deadline: Instant) -> Result<Self, BrokerFailure> {
        let transport = BrokerTransport::connect(options, deadline)?;
        let connection_options =
            ConnectionOptions::new().maximum_packet_size_recv(MQTT_MAX_PACKET_BYTES);
        let mut connection = mqtt::Connection::<mqtt::role::Client>::with_options(
            mqtt::Version::V5_0,
            connection_options,
        );
        connection.set_auto_pub_response(true);
        connection.set_pingresp_recv_timeout(options.pingresp_timeout_ms);
        if let Some(interval_ms) = options.pingreq_interval_override_ms {
            let _ = connection.set_pingreq_send_interval(Some(interval_ms));
        }

        let mut active = Self {
            transport,
            connection,
            pingresp_timeout_ms: options.pingresp_timeout_ms,
            pingreq_send_at: None,
            pingresp_recv_at: None,
            pending_packets: VecDeque::new(),
            read_buffer: vec![0_u8; READ_BUFFER_BYTES],
        };
        let connect = options.build_connect_packet()?;
        let events = active.connection.checked_send(connect);
        if active.apply_events(events, deadline)? {
            return Err(network_unavailable());
        }
        match active.next_packet_until(deadline, false)? {
            Some(Packet::V5_0Connack(connack))
                if connack.reason_code() == ConnectReasonCode::Success =>
            {
                Ok(active)
            }
            Some(Packet::V5_0Connack(connack)) => Err(connack_failure(connack.reason_code())),
            Some(_) => Err(protocol_settings_failure()),
            None => Err(timeout_failure()),
        }
    }

    fn publish(
        &mut self,
        publication: &Publication,
        deadline: Instant,
    ) -> Result<(), BrokerFailure> {
        let qos_level = qos(publication.qos);
        let packet_id = if qos_level == Qos::AtMostOnce {
            None
        } else {
            Some(
                self.connection
                    .acquire_packet_id()
                    .map_err(|_| protocol_settings_failure())?,
            )
        };
        let packet = build_publish_packet(publication, qos_level, packet_id)?;
        let events = self.connection.checked_send(packet);
        if self.apply_events(events, deadline)? {
            return Err(network_unavailable());
        }
        let Some(packet_id) = packet_id else {
            return Ok(());
        };
        self.wait_for_publish_ack(qos_level, packet_id, deadline)
    }

    fn wait_for_publish_ack(
        &mut self,
        qos_level: Qos,
        packet_id: u16,
        deadline: Instant,
    ) -> Result<(), BrokerFailure> {
        debug_assert!(
            qos_level != Qos::AtMostOnce,
            "QoS 0 publications do not wait for broker acknowledgements"
        );
        loop {
            let Some(packet) = self.next_packet_until(deadline, false)? else {
                return Err(timeout_failure());
            };
            match packet {
                Packet::V5_0Puback(ack)
                    if qos_level == Qos::AtLeastOnce && ack.packet_id() == packet_id =>
                {
                    return match ack.reason_code() {
                        None
                        | Some(
                            PubackReasonCode::Success | PubackReasonCode::NoMatchingSubscribers,
                        ) => Ok(()),
                        Some(_) => Err(publication_rejected()),
                    };
                }
                Packet::V5_0Pubrec(rec)
                    if qos_level == Qos::ExactlyOnce && rec.packet_id() == packet_id =>
                {
                    match rec.reason_code() {
                        None | Some(PubrecReasonCode::Success) => {}
                        // ponytail: mqtt-protocol-core 0.7.9 releases the packet ID on non-Success PUBREC instead of sending PUBREL; upgrade if a broker requires PUBREL on 0x10.
                        Some(PubrecReasonCode::NoMatchingSubscribers) => return Ok(()),
                        Some(_) => return Err(publication_rejected()),
                    }
                }
                Packet::V5_0Pubcomp(comp)
                    if qos_level == Qos::ExactlyOnce && comp.packet_id() == packet_id =>
                {
                    return match comp.reason_code() {
                        None | Some(PubcompReasonCode::Success) => Ok(()),
                        Some(PubcompReasonCode::PacketIdentifierNotFound) => {
                            Err(publication_rejected())
                        }
                    };
                }
                Packet::V5_0Disconnect(disconnect) => {
                    return Err(disconnect_failure(disconnect.reason_code()));
                }
                _ => {}
            }
        }
    }

    fn poll(&mut self, deadline: Instant) -> Result<(), BrokerFailure> {
        loop {
            match self.next_packet_until(deadline, true)? {
                None => return Ok(()),
                Some(Packet::V5_0Disconnect(disconnect)) => {
                    return Err(disconnect_failure(disconnect.reason_code()));
                }
                Some(_) => {}
            }
        }
    }

    fn disconnect(mut self, deadline: Instant) -> Result<(), BrokerFailure> {
        let packet = v5_0::Disconnect::builder()
            .reason_code(DisconnectReasonCode::NormalDisconnection)
            .build()
            .map_err(|_| protocol_settings_failure())?;
        let events = self.connection.checked_send(packet);
        let result = self.apply_events(events, deadline);
        self.transport.shutdown();
        result.map(|_| ())
    }

    fn next_packet_until(
        &mut self,
        deadline: Instant,
        timeout_is_ok: bool,
    ) -> Result<Option<Packet>, BrokerFailure> {
        loop {
            if let Some(packet) = self.pending_packets.pop_front() {
                return Ok(Some(packet));
            }
            let now = Instant::now();
            if !timeout_is_ok && now >= deadline {
                return Err(timeout_failure());
            }
            let write_deadline = self.write_deadline(now, deadline, timeout_is_ok);
            self.fire_due_timers(now, write_deadline)?;
            let now = Instant::now();
            if now >= deadline {
                return if timeout_is_ok {
                    Ok(None)
                } else {
                    Err(timeout_failure())
                };
            }
            let wait_until = self
                .next_timer_deadline()
                .map_or(deadline, |timer| timer.min(deadline));
            let wait = wait_until
                .saturating_duration_since(now)
                .max(Duration::from_millis(1));
            self.transport
                .set_read_timeout(Some(wait))
                .map_err(|_| network_unavailable())?;
            match self.transport.read(&mut self.read_buffer) {
                Ok(0) => {
                    let _ = self.connection.notify_closed();
                    return Err(network_unavailable());
                }
                Ok(bytes_read) => {
                    let feed_deadline =
                        self.write_deadline(Instant::now(), deadline, timeout_is_ok);
                    self.feed_received_bytes(bytes_read, feed_deadline)?;
                }
                Err(error)
                    if error.kind() == io::ErrorKind::Interrupted || is_timeout_error(&error) => {}
                Err(_) => return Err(network_unavailable()),
            }
        }
    }

    fn write_deadline(&self, now: Instant, deadline: Instant, timeout_is_ok: bool) -> Instant {
        if timeout_is_ok {
            deadline.max(now + Duration::from_millis(self.pingresp_timeout_ms))
        } else {
            deadline
        }
    }

    fn feed_received_bytes(
        &mut self,
        bytes_read: usize,
        deadline: Instant,
    ) -> Result<(), BrokerFailure> {
        assert!(
            bytes_read <= self.read_buffer.len(),
            "read byte count {bytes_read} exceeds buffer length {}",
            self.read_buffer.len()
        );
        let mut offset = 0_usize;
        while offset < bytes_read {
            let mut cursor = Cursor::new(&self.read_buffer[offset..bytes_read]);
            let events = self.connection.recv(&mut cursor);
            let consumed = usize::try_from(cursor.position()).unwrap_or(bytes_read - offset);
            if self.apply_events(events, deadline)? {
                return Err(network_unavailable());
            }
            if consumed == 0 {
                break;
            }
            offset += consumed;
        }
        Ok(())
    }

    fn fire_due_timers(
        &mut self,
        now: Instant,
        write_deadline: Instant,
    ) -> Result<(), BrokerFailure> {
        if self.pingresp_recv_at.is_some_and(|at| now >= at) {
            self.pingresp_recv_at = None;
            let events = self.connection.notify_timer_fired(TimerKind::PingrespRecv);
            let disconnect_deadline = now + Duration::from_millis(KEEP_ALIVE_DISCONNECT_TIMEOUT_MS);
            let _ = self.apply_events(events, disconnect_deadline.min(write_deadline));
            return Err(timeout_failure());
        }
        if self.pingreq_send_at.is_some_and(|at| now >= at) {
            self.pingreq_send_at = None;
            let events = self.connection.notify_timer_fired(TimerKind::PingreqSend);
            if self.apply_events(events, write_deadline)? {
                return Err(network_unavailable());
            }
        }
        Ok(())
    }

    fn next_timer_deadline(&self) -> Option<Instant> {
        match (self.pingreq_send_at, self.pingresp_recv_at) {
            (Some(send), Some(recv)) => Some(send.min(recv)),
            (Some(send), None) => Some(send),
            (None, Some(recv)) => Some(recv),
            (None, None) => None,
        }
    }

    fn apply_events(
        &mut self,
        events: Vec<Event>,
        deadline: Instant,
    ) -> Result<bool, BrokerFailure> {
        let mut close_requested = false;
        for event in events {
            match event {
                Event::RequestSendPacket {
                    packet,
                    release_packet_id_if_send_error,
                } => {
                    let wire = packet.to_continuous_buffer();
                    if let Err(error) = self.write_all_until(&wire, deadline) {
                        if let Some(packet_id) = release_packet_id_if_send_error {
                            let _ = self.connection.release_packet_id(packet_id);
                        }
                        return Err(error);
                    }
                }
                Event::RequestTimerReset { kind, duration_ms } => {
                    let target = Instant::now() + Duration::from_millis(duration_ms);
                    match kind {
                        TimerKind::PingreqSend => self.pingreq_send_at = Some(target),
                        TimerKind::PingrespRecv => self.pingresp_recv_at = Some(target),
                        TimerKind::PingreqRecv => {}
                    }
                }
                Event::RequestTimerCancel(kind) => match kind {
                    TimerKind::PingreqSend => self.pingreq_send_at = None,
                    TimerKind::PingrespRecv => self.pingresp_recv_at = None,
                    TimerKind::PingreqRecv => {}
                },
                Event::NotifyPacketReceived(packet) => self.pending_packets.push_back(packet),
                Event::NotifyPacketIdReleased(_) => {}
                Event::NotifyError(error) => return Err(mqtt_error_failure(error)),
                Event::RequestClose => close_requested = true,
            }
        }
        Ok(close_requested)
    }

    fn write_all_until(
        &mut self,
        mut bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), BrokerFailure> {
        while !bytes.is_empty() {
            let remaining = remaining_duration(deadline)?;
            self.transport
                .set_write_timeout(Some(remaining))
                .map_err(|_| network_unavailable())?;
            match self.transport.write(bytes) {
                Ok(0) => return Err(network_unavailable()),
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if is_timeout_error(&error) => {
                    if Instant::now() >= deadline {
                        return Err(timeout_failure());
                    }
                }
                Err(_) => return Err(network_unavailable()),
            }
        }
        let remaining = remaining_duration(deadline)?;
        self.transport
            .set_write_timeout(Some(remaining))
            .map_err(|_| network_unavailable())?;
        self.transport.flush().map_err(|error| {
            if is_timeout_error(&error) {
                timeout_failure()
            } else {
                network_unavailable()
            }
        })
    }
}

pub(super) struct BrokerSession {
    options: BrokerOptions,
    active: Option<ActiveSession>,
}

impl BrokerSession {
    pub(super) fn new(config: &MqttForwarderConfig) -> Result<Self, BrokerFailure> {
        let options = BrokerOptions::from_config(config)?;
        Ok(Self {
            options,
            active: None,
        })
    }

    pub(super) fn connect(&mut self, timeout: Duration) -> Result<(), BrokerFailure> {
        let deadline = Instant::now() + timeout;
        self.connect_until(deadline)
    }

    fn connect_until(&mut self, deadline: Instant) -> Result<(), BrokerFailure> {
        if self.active.is_some() {
            return Ok(());
        }
        let session = ActiveSession::open(&self.options, deadline)?;
        self.active = Some(session);
        Ok(())
    }

    pub(super) fn publish(
        &mut self,
        publication: &Publication,
        timeout: Duration,
    ) -> Result<(), BrokerFailure> {
        let deadline = Instant::now() + timeout;
        self.connect_until(deadline)?;
        let active = self
            .active
            .as_mut()
            .expect("MQTT session must be active after connect_until succeeds");
        let result = active.publish(publication, deadline);
        if result.is_err() {
            self.active = None;
        }
        result
    }

    pub(super) fn poll(&mut self, timeout: Duration) -> Result<(), BrokerFailure> {
        let Some(active) = self.active.as_mut() else {
            return Err(network_unavailable());
        };
        let deadline = Instant::now() + timeout;
        let result = active.poll(deadline);
        if result.is_err() {
            self.active = None;
        }
        result
    }

    pub(super) fn disconnect(&mut self, timeout: Duration) -> Result<(), BrokerFailure> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        let deadline = Instant::now() + timeout;
        active.disconnect(deadline)
    }
}

pub(super) fn probe(config: &MqttForwarderConfig, timeout: Duration) -> Result<(), BrokerFailure> {
    let mut probe_config = config.clone();
    probe_config.client_id = probe_client_id(&config.client_id);
    let mut session = BrokerSession::new(&probe_config)?;
    let publication = Publication {
        dedup_key: "probe".to_owned(),
        topic: status_topic(config),
        payload: serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "state": "test",
            "forwarder_id": config.forwarder_id,
        }))
        .map_err(|_| BrokerFailure {
            kind: BrokerFailureKind::Protocol,
            detail: "MQTT test status could not be encoded.".to_owned(),
        })?,
        qos: config.qos,
        retain: false,
        event_timestamp_ms: 0,
        content_type: "application/json".to_owned(),
        payload_format_indicator: Some(1),
        correlation_data: config.forwarder_id.as_bytes().to_vec(),
    };
    session.publish(&publication, timeout)?;
    session.disconnect(timeout)
}

fn probe_client_id(client_id: &str) -> String {
    let suffix = format!("-test-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let maximum_prefix_bytes = 128_usize.saturating_sub(suffix.len());
    let prefix = client_id
        .char_indices()
        .take_while(|(index, character)| index + character.len_utf8() <= maximum_prefix_bytes)
        .map(|(_, character)| character)
        .collect::<String>();
    format!("{prefix}{suffix}")
}

fn connect_tcp(host: &str, port: u16, deadline: Instant) -> Result<TcpStream, BrokerFailure> {
    let addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|_| network_unavailable())?
        .take(CONNECT_ADDRESS_COUNT_MAX)
        .collect();
    if addresses.is_empty() {
        return Err(network_unavailable());
    }
    let mut saw_timeout = false;
    for (index, address) in addresses.iter().enumerate() {
        let remaining = remaining_duration(deadline)?;
        let remaining_addresses = u32::try_from(addresses.len() - index).unwrap_or(1);
        let slice = if remaining_addresses > 1 {
            (remaining / remaining_addresses)
                .max(Duration::from_millis(CONNECT_SLICE_MS_MIN))
                .min(remaining)
        } else {
            remaining
        };
        match TcpStream::connect_timeout(address, slice) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) if is_timeout_error(&error) => {
                saw_timeout = true;
            }
            Err(_) => {}
        }
    }
    if saw_timeout || Instant::now() >= deadline {
        Err(timeout_failure())
    } else {
        Err(network_unavailable())
    }
}

fn build_tls_connector(ca_pem: Option<&[u8]>) -> Result<TlsConnector, BrokerFailure> {
    let mut builder = TlsConnector::builder();
    if let Some(pem_bytes) = ca_pem {
        let certificates = parse_pem_certificates(pem_bytes)?;
        builder.disable_built_in_roots(true);
        for certificate in certificates {
            builder.add_root_certificate(certificate);
        }
    }
    builder.build().map_err(|_| tls_validation_failure())
}

fn parse_pem_certificates(pem_bytes: &[u8]) -> Result<Vec<Certificate>, BrokerFailure> {
    let text = std::str::from_utf8(pem_bytes).map_err(|_| tls_validation_failure())?;
    let mut certificates = Vec::new();
    let mut cursor = text;
    while let Some(start) = cursor.find(PEM_CERT_BEGIN) {
        if certificates.len() >= CA_CERTIFICATE_COUNT_MAX {
            return Err(tls_validation_failure());
        }
        let block_start = &cursor[start..];
        let Some(end_offset) = block_start.find(PEM_CERT_END) else {
            return Err(tls_validation_failure());
        };
        let end = end_offset + PEM_CERT_END.len();
        let certificate = Certificate::from_pem(&block_start.as_bytes()[..end])
            .map_err(|_| tls_validation_failure())?;
        certificates.push(certificate);
        cursor = &block_start[end..];
    }
    if certificates.is_empty() {
        return Err(tls_validation_failure());
    }
    Ok(certificates)
}

fn finish_tls_handshake(
    connector: &TlsConnector,
    host: &str,
    tcp: TcpStream,
    deadline: Instant,
) -> Result<TlsStream<TcpStream>, BrokerFailure> {
    let remaining = remaining_duration(deadline)?;
    tcp.set_read_timeout(Some(remaining))
        .map_err(|_| network_unavailable())?;
    tcp.set_write_timeout(Some(remaining))
        .map_err(|_| network_unavailable())?;
    let mut result = connector.connect(host, tcp);
    for _ in 0..TLS_HANDSHAKE_STEP_COUNT_MAX {
        match result {
            Ok(stream) => return Ok(stream),
            Err(HandshakeError::WouldBlock(mut mid)) => {
                let remaining = remaining_duration(deadline)?;
                mid.get_mut()
                    .set_read_timeout(Some(remaining))
                    .map_err(|_| network_unavailable())?;
                mid.get_mut()
                    .set_write_timeout(Some(remaining))
                    .map_err(|_| network_unavailable())?;
                result = mid.handshake();
            }
            Err(HandshakeError::Failure(_)) => {
                return if Instant::now() >= deadline {
                    Err(timeout_failure())
                } else {
                    Err(tls_validation_failure())
                };
            }
        }
    }
    Err(timeout_failure())
}

fn build_publish_packet(
    publication: &Publication,
    qos_level: Qos,
    packet_id: Option<u16>,
) -> Result<v5_0::Publish, BrokerFailure> {
    let mut props = Vec::with_capacity(4);
    if let Some(indicator) = publication.payload_format_indicator {
        let format = match indicator {
            0 => PayloadFormat::Binary,
            1 => PayloadFormat::String,
            _ => return Err(protocol_settings_failure()),
        };
        props.push(Property::PayloadFormatIndicator(
            PayloadFormatIndicator::new(format).map_err(|_| protocol_settings_failure())?,
        ));
    }
    props.push(Property::ContentType(
        ContentType::new(publication.content_type.as_str())
            .map_err(|_| protocol_settings_failure())?,
    ));
    props.push(Property::CorrelationData(
        CorrelationData::new(&publication.correlation_data)
            .map_err(|_| protocol_settings_failure())?,
    ));
    props.push(Property::UserProperty(
        UserProperty::new("schema-version", "1").map_err(|_| protocol_settings_failure())?,
    ));

    let mut builder = v5_0::Publish::builder()
        .topic_name(publication.topic.as_str())
        .map_err(|_| protocol_settings_failure())?
        .qos(qos_level)
        .retain(publication.retain)
        .payload(publication.payload.as_slice())
        .props(props);
    if let Some(id) = packet_id {
        builder = builder.packet_id(id);
    }
    builder.build().map_err(|_| protocol_settings_failure())
}

const fn qos(value: u8) -> Qos {
    match value {
        0 => Qos::AtMostOnce,
        2 => Qos::ExactlyOnce,
        _ => Qos::AtLeastOnce,
    }
}

fn remaining_duration(deadline: Instant) -> Result<Duration, BrokerFailure> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(timeout_failure)
}

fn is_timeout_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

fn timeout_failure() -> BrokerFailure {
    BrokerFailure {
        kind: BrokerFailureKind::Timeout,
        detail: "MQTT broker did not acknowledge the operation before its deadline.".to_owned(),
    }
}

fn publication_rejected() -> BrokerFailure {
    BrokerFailure {
        kind: BrokerFailureKind::Protocol,
        detail: "MQTT 5 broker rejected the publication.".to_owned(),
    }
}

fn protocol_settings_failure() -> BrokerFailure {
    BrokerFailure {
        kind: BrokerFailureKind::Protocol,
        detail: "MQTT 5 broker rejected the client protocol settings.".to_owned(),
    }
}

fn tls_validation_failure() -> BrokerFailure {
    BrokerFailure {
        kind: BrokerFailureKind::Tls,
        detail: "MQTT TLS validation failed; verify the broker hostname and CA trust.".to_owned(),
    }
}

fn network_unavailable() -> BrokerFailure {
    BrokerFailure {
        kind: BrokerFailureKind::Network,
        detail: "MQTT 5 broker is unavailable.".to_owned(),
    }
}

fn connack_failure(code: ConnectReasonCode) -> BrokerFailure {
    match code {
        ConnectReasonCode::BadUserNameOrPassword
        | ConnectReasonCode::NotAuthorized
        | ConnectReasonCode::BadAuthenticationMethod => BrokerFailure {
            kind: BrokerFailureKind::Authentication,
            detail: "MQTT broker rejected the configured credentials.".to_owned(),
        },
        ConnectReasonCode::ServerUnavailable
        | ConnectReasonCode::ServerBusy
        | ConnectReasonCode::UseAnotherServer
        | ConnectReasonCode::ServerMoved
        | ConnectReasonCode::ConnectionRateExceeded => network_unavailable(),
        _ => BrokerFailure {
            kind: BrokerFailureKind::Protocol,
            detail: "MQTT 5 broker rejected the connection settings.".to_owned(),
        },
    }
}

fn disconnect_failure(code: Option<DisconnectReasonCode>) -> BrokerFailure {
    match code {
        Some(DisconnectReasonCode::NotAuthorized) => BrokerFailure {
            kind: BrokerFailureKind::Authentication,
            detail: "MQTT broker rejected the configured credentials.".to_owned(),
        },
        Some(DisconnectReasonCode::KeepAliveTimeout | DisconnectReasonCode::MaximumConnectTime) => {
            timeout_failure()
        }
        Some(
            DisconnectReasonCode::NormalDisconnection
            | DisconnectReasonCode::ServerBusy
            | DisconnectReasonCode::ServerShuttingDown
            | DisconnectReasonCode::SessionTakenOver
            | DisconnectReasonCode::UseAnotherServer
            | DisconnectReasonCode::ServerMoved
            | DisconnectReasonCode::ConnectionRateExceeded
            | DisconnectReasonCode::AdministrativeAction,
        )
        | None => network_unavailable(),
        Some(_) => protocol_settings_failure(),
    }
}

fn mqtt_error_failure(error: MqttError) -> BrokerFailure {
    match error {
        MqttError::BadUserNameOrPassword
        | MqttError::NotAuthorized
        | MqttError::BadAuthenticationMethod => BrokerFailure {
            kind: BrokerFailureKind::Authentication,
            detail: "MQTT broker rejected the configured credentials.".to_owned(),
        },
        MqttError::KeepAliveTimeout | MqttError::MaximumConnectTime => timeout_failure(),
        MqttError::ServerUnavailable
        | MqttError::ServerBusy
        | MqttError::ServerShuttingDown
        | MqttError::UseAnotherServer
        | MqttError::ServerMoved
        | MqttError::ConnectionRateExceeded => network_unavailable(),
        _ => protocol_settings_failure(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, path::PathBuf};

    #[test]
    fn builds_plain_and_authenticated_tls_options_without_exposing_credentials() {
        let plain = BrokerOptions::from_config(&MqttForwarderConfig::default()).unwrap();
        assert_eq!((plain.host.as_str(), plain.port), ("127.0.0.1", 1883));
        assert!(!plain.use_tls);

        let missing_ca = PathBuf::from("/definitely/missing/keeppeek-mqtt-ca.pem");
        let tls = MqttForwarderConfig {
            broker_url: "mqtts://broker.example".to_owned(),
            username: Some("operator".to_owned()),
            password: Some("super-secret".to_owned()),
            tls_ca_path: Some(missing_ca),
            ..MqttForwarderConfig::default()
        };
        let error = BrokerOptions::from_config(&tls).unwrap_err();
        assert_eq!(error.kind, BrokerFailureKind::Tls);
        assert!(!error.detail.contains("operator"));
        assert!(!error.detail.contains("super-secret"));
        assert!(!error.detail.contains("missing"));

        let authenticated = MqttForwarderConfig {
            broker_url: "mqtts://broker.example".to_owned(),
            username: Some("operator".to_owned()),
            password: Some("super-secret".to_owned()),
            ..MqttForwarderConfig::default()
        };
        let debug_text = format!("{:?}", BrokerOptions::from_config(&authenticated).unwrap());
        assert!(!debug_text.contains("operator"));
        assert!(!debug_text.contains("super-secret"));
    }

    #[test]
    fn classifies_credentials_and_tls_without_leaking_transport_errors() {
        let authentication = connack_failure(ConnectReasonCode::BadUserNameOrPassword);
        assert_eq!(authentication.kind, BrokerFailureKind::Authentication);
        assert_eq!(
            authentication.detail,
            "MQTT broker rejected the configured credentials."
        );
    }

    #[test]
    fn connection_waits_for_connack_within_operation_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let mut stream = accept_before_deadline(&listener);
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let (_, connect) = read_frame(&mut stream);
            assert_eq!(connect[6], 5);
            // Delay CONNACK beyond the former 100 ms polling interval.
            std::thread::sleep(Duration::from_millis(250));
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        });
        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            ..MqttForwarderConfig::default()
        };
        let mut session = BrokerSession::new(&config).unwrap();
        let result = session.connect(Duration::from_secs(2));
        broker.join().unwrap();
        result.unwrap();
    }

    #[test]
    fn connection_without_connack_stops_at_operation_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let mut stream = accept_before_deadline(&listener);
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let (_, connect) = read_frame(&mut stream);
            assert_eq!(connect[6], 5);
            let mut closed = [0_u8; 1];
            assert_eq!(stream.read(&mut closed).unwrap(), 0);
        });
        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            ..MqttForwarderConfig::default()
        };
        let mut session = BrokerSession::new(&config).unwrap();
        let result = session.connect(Duration::from_millis(250));
        broker.join().unwrap();
        assert_eq!(result.unwrap_err().kind, BrokerFailureKind::Timeout);
    }

    #[test]
    #[cfg(not(windows))]
    fn authenticated_tls_probe_uses_configured_ca_and_credentials() {
        #[cfg(target_os = "macos")]
        // Isolate Secure Transport because native-tls server handshakes can fail in a parallel macOS test process (rust-native-tls/rust-native-tls#34).
        if std::env::var_os("KEEPPEEK_MQTT_TLS_TEST_CHILD").is_none() {
            let current_thread = std::thread::current();
            let test_name = current_thread
                .name()
                .expect("the Rust test harness must name its test threads");
            let status = std::process::Command::new(
                std::env::current_exe().expect("the current test executable must be available"),
            )
            .args([test_name, "--exact", "--test-threads=1"])
            .env("KEEPPEEK_MQTT_TLS_TEST_CHILD", "1")
            .status()
            .expect("the isolated MQTT TLS test process must start");
            assert!(status.success(), "the isolated MQTT TLS test failed");
            return;
        }

        let mut ca_params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "KeepPeek MQTT Test CA");
        ca_params.key_usages.extend([
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ]);
        let ca_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let mut server_params =
            rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        server_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "localhost");
        server_params.use_authority_key_identifier_extension = true;
        server_params.key_usages.extend([
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyEncipherment,
        ]);
        server_params
            .extended_key_usages
            .push(rcgen::ExtendedKeyUsagePurpose::ServerAuth);
        let signing_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).unwrap();
        let cert = server_params.signed_by(&signing_key, &issuer).unwrap();
        let certificate_chain = format!("{}{}", cert.pem(), ca_cert.pem());
        let identity = native_tls::Identity::from_pkcs8(
            certificate_chain.as_bytes(),
            signing_key.serialize_pem().as_bytes(),
        )
        .unwrap();
        let acceptor = native_tls::TlsAcceptor::new(identity).unwrap();
        let ca_path =
            std::env::temp_dir().join(format!("keeppeek-mqtt-ca-{}.pem", uuid::Uuid::new_v4()));
        std::fs::write(&ca_path, ca_cert.pem()).unwrap();
        let listener = TcpListener::bind("localhost:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut stream = acceptor.accept(stream).unwrap();
            let (_, connect) = read_frame(&mut stream);
            let (username, password) = connect_credentials(&connect);
            assert_eq!(username.as_deref(), Some("operator"));
            assert_eq!(password.as_deref(), Some(b"correct-horse".as_slice()));
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
            let (header, publish) = read_frame(&mut stream);
            let packet_id = publish_packet_id(header, &publish)
                .expect("test publication must request an acknowledgement")
                .to_be_bytes();
            stream
                .write_all(&[0x40, 0x02, packet_id[0], packet_id[1]])
                .unwrap();
            let (disconnect_header, _) = read_frame(&mut stream);
            assert_eq!(disconnect_header >> 4, 14);
        });
        let config = MqttForwarderConfig {
            broker_url: format!("mqtts://localhost:{}", address.port()),
            username: Some("operator".to_owned()),
            password: Some("correct-horse".to_owned()),
            tls_ca_path: Some(ca_path.clone()),
            ..MqttForwarderConfig::default()
        };

        let result = probe(&config, Duration::from_secs(3));
        let broker_result = broker.join();
        let _ = std::fs::remove_file(ca_path);
        assert!(
            result.is_ok() && broker_result.is_ok(),
            "client={result:?}, broker={broker_result:?}"
        );
    }

    #[test]
    fn broker_rejected_credentials_are_actionable_and_secret_safe() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (_, connect) = read_frame(&mut stream);
            let (username, password) = connect_credentials(&connect);
            assert_eq!(username.as_deref(), Some("operator"));
            assert_eq!(password.as_deref(), Some(b"wrong-secret".as_slice()));
            stream.write_all(&[0x20, 0x03, 0x00, 0x86, 0x00]).unwrap();
        });
        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            username: Some("operator".to_owned()),
            password: Some("wrong-secret".to_owned()),
            ..MqttForwarderConfig::default()
        };

        let error = probe(&config, Duration::from_secs(2)).unwrap_err();
        broker.join().unwrap();
        assert_eq!(error.kind, BrokerFailureKind::Authentication);
        assert_eq!(
            error.detail,
            "MQTT broker rejected the configured credentials."
        );
        assert!(!error.detail.contains("wrong-secret"));
    }

    #[test]
    fn invalid_tls_trust_is_actionable_and_does_not_expose_paths() {
        let listener = TcpListener::bind("localhost:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut closed = [0_u8; 1];
            let _ = stream.read(&mut closed);
        });
        let ca_path = std::env::temp_dir().join(format!(
            "keeppeek-invalid-mqtt-ca-{}.pem",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&ca_path, "not a certificate").unwrap();
        let config = MqttForwarderConfig {
            broker_url: format!("mqtts://localhost:{}", address.port()),
            tls_ca_path: Some(ca_path.clone()),
            ..MqttForwarderConfig::default()
        };

        let error = probe(&config, Duration::from_secs(2)).unwrap_err();
        broker.join().unwrap();
        let _ = std::fs::remove_file(&ca_path);
        assert_eq!(error.kind, BrokerFailureKind::Tls);
        assert_eq!(
            error.detail,
            "MQTT TLS validation failed; verify the broker hostname and CA trust."
        );
        assert!(
            !error
                .detail
                .contains(&ca_path.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn probe_uses_only_mqtt_five_with_required_publish_properties() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (connect_header, connect) = read_frame(&mut stream);
            assert_eq!(connect_header, 0x10);
            assert_eq!(&connect[2..6], b"MQTT");
            assert_eq!(connect[6], 5, "client must send MQTT protocol level 5");
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();

            let (publish_header, publish) = read_frame(&mut stream);
            assert_eq!(publish_header >> 4, 3);
            let topic_length = usize::from(u16::from_be_bytes([publish[0], publish[1]]));
            let topic_end = 2 + topic_length;
            assert_eq!(
                std::str::from_utf8(&publish[2..topic_end]).unwrap(),
                "keeppeek/home-nvr/forwarders/mqtt/status"
            );
            let packet_id = u16::from_be_bytes([publish[topic_end], publish[topic_end + 1]]);
            let properties_start = topic_end + 2;
            let (properties_length, properties_prefix_length) =
                variable_integer(&publish[properties_start..]);
            let properties = &publish[properties_start + properties_prefix_length
                ..properties_start + properties_prefix_length + properties_length];
            assert!(contains_bytes(properties, &[0x01, 0x01]));
            assert!(contains_bytes(
                properties,
                &[
                    0x03, 0x00, 0x10, b'a', b'p', b'p', b'l', b'i', b'c', b'a', b't', b'i', b'o',
                    b'n', b'/', b'j', b's', b'o', b'n'
                ]
            ));
            assert!(contains_bytes(
                properties,
                &[0x09, 0x00, 0x04, b'm', b'q', b't', b't']
            ));
            let packet_id = packet_id.to_be_bytes();
            stream
                .write_all(&[0x40, 0x02, packet_id[0], packet_id[1]])
                .unwrap();
            let (disconnect_header, _) = read_frame(&mut stream);
            assert_eq!(disconnect_header >> 4, 14);
        });

        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            ..MqttForwarderConfig::default()
        };
        let result = probe(&config, Duration::from_secs(2));
        broker.join().unwrap();
        result.unwrap();
    }

    #[test]
    fn mqtt_five_rejection_does_not_fallback_to_mqtt_three() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let (_, connect) = read_frame(&mut stream);
            assert_eq!(connect[6], 5);
            stream.write_all(&[0x20, 0x03, 0x00, 0x84, 0x00]).unwrap();
        });

        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            ..MqttForwarderConfig::default()
        };
        let error = probe(&config, Duration::from_secs(2)).unwrap_err();
        assert_eq!(error.kind, BrokerFailureKind::Protocol);
        assert!(error.detail.contains("MQTT 5"));
        broker.join().unwrap();
    }

    #[test]
    fn qos_zero_and_qos_two_complete_expected_mqtt_five_handshakes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let mut stream = accept_before_deadline(&listener);
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let (_, connect) = read_frame(&mut stream);
            assert_eq!(connect[6], 5);
            // Send CONNACK in fragmented chunks to exercise streaming reassembly.
            stream.write_all(&[0x20, 0x03]).unwrap();
            std::thread::sleep(Duration::from_millis(10));
            stream.write_all(&[0x00, 0x00, 0x00]).unwrap();

            let (qos0_header, qos0_body) = read_frame(&mut stream);
            assert_eq!(qos0_header >> 4, 3);
            assert_eq!((qos0_header >> 1) & 0x03, 0);
            assert!(publish_packet_id(qos0_header, &qos0_body).is_none());

            let (qos2_header, qos2_body) = read_frame(&mut stream);
            assert_eq!(qos2_header >> 4, 3);
            assert_eq!((qos2_header >> 1) & 0x03, 2);
            let packet_id = publish_packet_id(qos2_header, &qos2_body)
                .expect("QoS 2 publish must carry a packet identifier");
            let id_bytes = packet_id.to_be_bytes();
            stream
                .write_all(&[0x50, 0x02, id_bytes[0], id_bytes[1]])
                .unwrap();

            let (pubrel_header, pubrel_body) = read_frame(&mut stream);
            assert_eq!(pubrel_header, 0x62);
            assert_eq!(&pubrel_body[..2], &id_bytes);
            stream
                .write_all(&[0x70, 0x02, id_bytes[0], id_bytes[1]])
                .unwrap();

            let (disconnect_header, _) = read_frame(&mut stream);
            assert_eq!(disconnect_header >> 4, 14);
        });

        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            ..MqttForwarderConfig::default()
        };
        let mut session = BrokerSession::new(&config).unwrap();
        let qos0 = Publication {
            dedup_key: "qos0".to_owned(),
            topic: "keeppeek/home-nvr/sources/cam/events/motion".to_owned(),
            payload: b"{}".to_vec(),
            qos: 0,
            retain: false,
            event_timestamp_ms: 1,
            content_type: "application/json".to_owned(),
            payload_format_indicator: Some(1),
            correlation_data: b"evt-0".to_vec(),
        };
        let qos2 = Publication {
            dedup_key: "qos2".to_owned(),
            qos: 2,
            correlation_data: b"evt-2".to_vec(),
            ..qos0.clone()
        };
        session.publish(&qos0, Duration::from_secs(2)).unwrap();
        session.publish(&qos2, Duration::from_secs(2)).unwrap();
        session.disconnect(Duration::from_secs(2)).unwrap();
        broker.join().unwrap();
    }

    #[test]
    fn poll_sends_keepalive_ping_and_detects_broker_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let mut stream = accept_before_deadline(&listener);
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let (_, connect) = read_frame(&mut stream);
            assert_eq!(connect[6], 5);
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();

            let (ping_header, ping_body) = read_frame(&mut stream);
            assert_eq!(ping_header, 0xC0);
            assert!(ping_body.is_empty());
            stream.write_all(&[0xD0, 0x00]).unwrap();

            // Send server DISCONNECT (ServerShuttingDown = 0x8B).
            stream.write_all(&[0xE0, 0x01, 0x8B]).unwrap();
        });

        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            ..MqttForwarderConfig::default()
        };
        let mut session = BrokerSession::new(&config).unwrap();
        session.options.pingreq_interval_override_ms = Some(40);
        session.connect(Duration::from_secs(2)).unwrap();
        let error = session.poll(Duration::from_secs(2)).unwrap_err();
        broker.join().unwrap();
        assert_eq!(error.kind, BrokerFailureKind::Network);
        assert!(session.active.is_none());
    }

    #[test]
    fn puback_and_pubrec_rejections_fail_with_protocol_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let mut stream = accept_before_deadline(&listener);
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let (_, _) = read_frame(&mut stream);
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
            let (header, publish) = read_frame(&mut stream);
            let packet_id = publish_packet_id(header, &publish).unwrap().to_be_bytes();
            // Reject QoS 1 publish with NotAuthorized (0x87).
            stream
                .write_all(&[0x40, 0x03, packet_id[0], packet_id[1], 0x87])
                .unwrap();
        });

        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            ..MqttForwarderConfig::default()
        };
        let error = probe(&config, Duration::from_secs(2)).unwrap_err();
        broker.join().unwrap();
        assert_eq!(error.kind, BrokerFailureKind::Protocol);
        assert_eq!(error.detail, "MQTT 5 broker rejected the publication.");
    }

    #[test]
    fn repeated_short_polls_complete_keepalive_and_timeout_when_pingresp_missing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let mut stream = accept_before_deadline(&listener);
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let (_, connect) = read_frame(&mut stream);
            assert_eq!(connect[6], 5);
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();

            // Respond to the first keep-alive PINGREQ, then ignore the second.
            let (first_ping, _) = read_frame(&mut stream);
            assert_eq!(first_ping, 0xC0);
            stream.write_all(&[0xD0, 0x00]).unwrap();

            let (second_ping, _) = read_frame(&mut stream);
            assert_eq!(second_ping, 0xC0);

            let (disconnect_header, disconnect_body) = read_frame(&mut stream);
            assert_eq!(disconnect_header >> 4, 14);
            assert_eq!(disconnect_body.first().copied(), Some(0x8D));
        });

        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            ..MqttForwarderConfig::default()
        };
        let mut session = BrokerSession::new(&config).unwrap();
        session.options.pingreq_interval_override_ms = Some(60);
        session.options.pingresp_timeout_ms = 40;
        session.connect(Duration::from_secs(2)).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let error = loop {
            assert!(Instant::now() < deadline, "missing PINGRESP must time out");
            if let Err(error) = session.poll(Duration::from_millis(15)) {
                break error;
            }
        };
        broker.join().unwrap();
        assert_eq!(error.kind, BrokerFailureKind::Timeout);
        assert!(session.active.is_none());
    }

    fn accept_before_deadline(listener: &TcpListener) -> std::net::TcpStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    return stream;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("MQTT fixture accept failed: {error}"),
            }
        }
        panic!("MQTT client did not connect before the fixture deadline");
    }

    fn read_frame(stream: &mut impl Read) -> (u8, Vec<u8>) {
        let mut header = [0_u8; 1];
        stream.read_exact(&mut header).unwrap();
        let mut multiplier = 1_usize;
        let mut remaining = 0_usize;
        loop {
            let mut encoded = [0_u8; 1];
            stream.read_exact(&mut encoded).unwrap();
            remaining += usize::from(encoded[0] & 0x7f) * multiplier;
            if encoded[0] & 0x80 == 0 {
                break;
            }
            multiplier *= 128;
        }
        let mut body = vec![0_u8; remaining];
        stream.read_exact(&mut body).unwrap();
        (header[0], body)
    }

    fn variable_integer(bytes: &[u8]) -> (usize, usize) {
        let mut value = 0_usize;
        let mut multiplier = 1_usize;
        for (index, byte) in bytes.iter().copied().enumerate() {
            value += usize::from(byte & 0x7f) * multiplier;
            if byte & 0x80 == 0 {
                return (value, index + 1);
            }
            multiplier *= 128;
        }
        panic!("MQTT variable integer is incomplete");
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn connect_credentials(body: &[u8]) -> (Option<String>, Option<Vec<u8>>) {
        let protocol_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
        let mut cursor = 2 + protocol_length;
        assert_eq!(body[cursor], 5);
        cursor += 1;
        let flags = body[cursor];
        cursor += 3;
        let (properties_length, properties_prefix) = variable_integer(&body[cursor..]);
        cursor += properties_prefix + properties_length;
        let _client_id = mqtt_binary(body, &mut cursor);
        if flags & 0x04 != 0 {
            let (will_properties_length, will_properties_prefix) =
                variable_integer(&body[cursor..]);
            cursor += will_properties_prefix + will_properties_length;
            let _will_topic = mqtt_binary(body, &mut cursor);
            let _will_payload = mqtt_binary(body, &mut cursor);
        }
        let username =
            (flags & 0x80 != 0).then(|| String::from_utf8(mqtt_binary(body, &mut cursor)).unwrap());
        let password = (flags & 0x40 != 0).then(|| mqtt_binary(body, &mut cursor));
        (username, password)
    }

    fn mqtt_binary(body: &[u8], cursor: &mut usize) -> Vec<u8> {
        let length = usize::from(u16::from_be_bytes([body[*cursor], body[*cursor + 1]]));
        *cursor += 2;
        let value = body[*cursor..*cursor + length].to_vec();
        *cursor += length;
        value
    }

    fn publish_packet_id(header: u8, body: &[u8]) -> Option<u16> {
        if (header >> 1) & 0x03 == 0 {
            return None;
        }
        let topic_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
        let offset = 2 + topic_length;
        Some(u16::from_be_bytes([body[offset], body[offset + 1]]))
    }
}
