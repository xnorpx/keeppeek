use super::{
    BrokerOptions, MQTT_MAX_PACKET_BYTES, READ_BUFFER_BYTES, connack_failure, disconnect_failure,
    mqtt_error_failure, protocol_settings_failure, publication_rejected,
};
use crate::event_forwarder::{BrokerFailure, model::Publication};
use mqtt_protocol_core::mqtt::{
    packet::{
        ContentType, CorrelationData, DecodeResult, MaximumPacketSize, PayloadFormat,
        PayloadFormatIndicator, Property, Qos, SessionExpiryInterval, UserProperty,
        VariableByteInteger, v5_0,
    },
    result_code::{ConnectReasonCode, DisconnectReasonCode, PubackReasonCode, PubcompReasonCode},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientPhase {
    AwaitingConnack,
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InflightPublish {
    Puback { packet_id: u16 },
    Pubrec { packet_id: u16 },
    Pubcomp { packet_id: u16 },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ClientAction {
    Send(Vec<u8>),
    Connected,
    PublishComplete,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum TimerAction {
    None,
    SendPingreq(Vec<u8>),
    KeepAliveExpired(Vec<u8>),
}

#[derive(Debug)]
enum IncomingPacket {
    Connack(v5_0::Connack),
    Publish(v5_0::Publish),
    Puback(v5_0::Puback),
    Pubrec(v5_0::Pubrec),
    Pubrel(v5_0::Pubrel),
    Pubcomp(v5_0::Pubcomp),
    Pingresp,
    Disconnect(v5_0::Disconnect),
}

struct RawFrame {
    first_byte: u8,
    body: Vec<u8>,
}

enum DecodeState {
    FixedHeader,
    RemainingLength {
        first_byte: u8,
        vbi_bytes: [u8; 4],
        vbi_len: usize,
    },
    Body {
        first_byte: u8,
        remaining_len: usize,
        body: Vec<u8>,
    },
}

pub(super) struct FrameDecoder {
    state: DecodeState,
}

impl FrameDecoder {
    pub(super) const fn new() -> Self {
        Self {
            state: DecodeState::FixedHeader,
        }
    }

    fn decode_next(&mut self, input: &mut &[u8]) -> Result<Option<RawFrame>, BrokerFailure> {
        while !input.is_empty() {
            match &mut self.state {
                DecodeState::FixedHeader => {
                    let first_byte = input[0];
                    *input = &input[1..];
                    self.state = DecodeState::RemainingLength {
                        first_byte,
                        vbi_bytes: [0; 4],
                        vbi_len: 0,
                    };
                }
                DecodeState::RemainingLength {
                    first_byte,
                    vbi_bytes,
                    vbi_len,
                } => {
                    let first = *first_byte;
                    vbi_bytes[*vbi_len] = input[0];
                    *vbi_len += 1;
                    *input = &input[1..];
                    let snapshot = *vbi_bytes;
                    let len = *vbi_len;
                    if let Some(frame) = self.finish_remaining_length(first, &snapshot[..len])? {
                        return Ok(Some(frame));
                    }
                }
                DecodeState::Body {
                    first_byte,
                    remaining_len,
                    body,
                } => {
                    let needed = *remaining_len - body.len();
                    let take = needed.min(input.len());
                    body.extend_from_slice(&input[..take]);
                    *input = &input[take..];
                    if body.len() == *remaining_len {
                        let first = *first_byte;
                        let completed = std::mem::take(body);
                        self.state = DecodeState::FixedHeader;
                        return Ok(Some(RawFrame {
                            first_byte: first,
                            body: completed,
                        }));
                    }
                }
            }
        }
        Ok(None)
    }

    fn finish_remaining_length(
        &mut self,
        first_byte: u8,
        vbi_slice: &[u8],
    ) -> Result<Option<RawFrame>, BrokerFailure> {
        match VariableByteInteger::decode_stream(vbi_slice) {
            DecodeResult::Ok(vbi, consumed) => {
                if vbi.size() != consumed {
                    return Err(protocol_settings_failure());
                }
                let remaining_len = vbi.to_u32();
                let consumed_u32 = u32::try_from(consumed).unwrap_or(u32::MAX);
                let total_bytes = 1_u32
                    .saturating_add(consumed_u32)
                    .saturating_add(remaining_len);
                if total_bytes > MQTT_MAX_PACKET_BYTES {
                    return Err(protocol_settings_failure());
                }
                let body_len =
                    usize::try_from(remaining_len).map_err(|_| protocol_settings_failure())?;
                if body_len == 0 {
                    self.state = DecodeState::FixedHeader;
                    return Ok(Some(RawFrame {
                        first_byte,
                        body: Vec::new(),
                    }));
                }
                // Cap initial allocation so a bogus header cannot reserve 16 MiB upfront.
                let initial_capacity = body_len.min(READ_BUFFER_BYTES);
                self.state = DecodeState::Body {
                    first_byte,
                    remaining_len: body_len,
                    body: Vec::with_capacity(initial_capacity),
                };
                Ok(None)
            }
            DecodeResult::Incomplete => Ok(None),
            DecodeResult::Err(_) => Err(protocol_settings_failure()),
        }
    }
}

pub(super) struct MqttClientState {
    phase: ClientPhase,
    decoder: FrameDecoder,
    next_packet_id: u16,
    inflight: Option<InflightPublish>,
    keep_alive_ms: Option<u64>,
    pingresp_timeout_ms: u64,
    pingreq_send_at: Option<Instant>,
    pingresp_recv_at: Option<Instant>,
    max_packet_size_send: Option<u32>,
}

impl MqttClientState {
    pub(super) const fn new(keep_alive_ms: Option<u64>, pingresp_timeout_ms: u64) -> Self {
        Self {
            phase: ClientPhase::AwaitingConnack,
            decoder: FrameDecoder::new(),
            next_packet_id: 1,
            inflight: None,
            keep_alive_ms,
            pingresp_timeout_ms,
            pingreq_send_at: None,
            pingresp_recv_at: None,
            max_packet_size_send: None,
        }
    }

    pub(super) const fn pingresp_timeout_ms(&self) -> u64 {
        self.pingresp_timeout_ms
    }

    #[cfg(test)]
    pub(super) const fn keep_alive_ms(&self) -> Option<u64> {
        self.keep_alive_ms
    }

    pub(super) fn next_timer_deadline(&self) -> Option<Instant> {
        match (self.pingreq_send_at, self.pingresp_recv_at) {
            (Some(send), Some(recv)) => Some(send.min(recv)),
            (Some(send), None) => Some(send),
            (None, Some(recv)) => Some(recv),
            (None, None) => None,
        }
    }

    pub(super) fn start_connect(&self, packet: &v5_0::Connect) -> Result<Vec<u8>, BrokerFailure> {
        debug_assert_eq!(self.phase, ClientPhase::AwaitingConnack);
        Ok(packet.to_continuous_buffer())
    }

    pub(super) fn start_publish(
        &mut self,
        publication: &Publication,
        now: Instant,
    ) -> Result<(Vec<u8>, bool), BrokerFailure> {
        debug_assert_eq!(self.phase, ClientPhase::Connected);
        debug_assert!(self.inflight.is_none());
        let qos_level = qos(publication.qos);
        let packet_id = (qos_level != Qos::AtMostOnce).then(|| self.allocate_packet_id());
        let packet = build_publish_packet(publication, qos_level, packet_id)?;
        let wire = packet.to_continuous_buffer();
        self.check_outbound_size(&wire)?;
        self.inflight = match (qos_level, packet_id) {
            (Qos::AtMostOnce, None) => None,
            (Qos::AtLeastOnce, Some(id)) => Some(InflightPublish::Puback { packet_id: id }),
            (Qos::ExactlyOnce, Some(id)) => Some(InflightPublish::Pubrec { packet_id: id }),
            _ => return Err(protocol_settings_failure()),
        };
        self.note_packet_sent(now);
        Ok((wire, self.inflight.is_some()))
    }

    pub(super) fn start_disconnect(&mut self) -> Result<Vec<u8>, BrokerFailure> {
        self.phase = ClientPhase::Disconnected;
        self.pingreq_send_at = None;
        self.pingresp_recv_at = None;
        let packet = v5_0::Disconnect::builder()
            .reason_code(DisconnectReasonCode::NormalDisconnection)
            .build()
            .map_err(|_| protocol_settings_failure())?;
        let wire = packet.to_continuous_buffer();
        self.check_outbound_size(&wire)?;
        Ok(wire)
    }

    pub(super) fn feed_bytes(
        &mut self,
        mut bytes: &[u8],
        now: Instant,
    ) -> Result<Vec<ClientAction>, BrokerFailure> {
        let mut actions = Vec::new();
        while let Some(frame) = self.decoder.decode_next(&mut bytes)? {
            let packet = parse_incoming_frame(frame)?;
            self.handle_packet(packet, now, &mut actions)?;
        }
        Ok(actions)
    }

    pub(super) fn fire_due_timers(&mut self, now: Instant) -> TimerAction {
        if self.pingresp_recv_at.is_some_and(|at| now >= at) {
            self.phase = ClientPhase::Disconnected;
            self.pingreq_send_at = None;
            self.pingresp_recv_at = None;
            let disconnect = v5_0::Disconnect::builder()
                .reason_code(DisconnectReasonCode::KeepAliveTimeout)
                .build()
                .map(|packet| packet.to_continuous_buffer())
                .unwrap_or_default();
            return TimerAction::KeepAliveExpired(disconnect);
        }
        if self.pingreq_send_at.is_some_and(|at| now >= at) {
            self.pingreq_send_at = self.keep_alive_ms.map(|ms| now + Duration::from_millis(ms));
            if self.pingresp_recv_at.is_none() {
                self.pingresp_recv_at = Some(now + Duration::from_millis(self.pingresp_timeout_ms));
                let ping = v5_0::Pingreq::new().to_continuous_buffer();
                return TimerAction::SendPingreq(ping);
            }
        }
        TimerAction::None
    }

    fn handle_packet(
        &mut self,
        packet: IncomingPacket,
        now: Instant,
        actions: &mut Vec<ClientAction>,
    ) -> Result<(), BrokerFailure> {
        match self.phase {
            ClientPhase::AwaitingConnack => self.handle_connack_packet(packet, now, actions),
            ClientPhase::Connected => self.handle_connected_packet(packet, now, actions),
            ClientPhase::Disconnected => Err(protocol_settings_failure()),
        }
    }

    fn handle_connack_packet(
        &mut self,
        packet: IncomingPacket,
        now: Instant,
        actions: &mut Vec<ClientAction>,
    ) -> Result<(), BrokerFailure> {
        let IncomingPacket::Connack(connack) = packet else {
            return Err(protocol_settings_failure());
        };
        if connack.reason_code() != ConnectReasonCode::Success {
            self.phase = ClientPhase::Disconnected;
            return Err(connack_failure(connack.reason_code()));
        }
        self.apply_connack_properties(&connack)?;
        self.phase = ClientPhase::Connected;
        self.pingreq_send_at = self.keep_alive_ms.map(|ms| now + Duration::from_millis(ms));
        actions.push(ClientAction::Connected);
        Ok(())
    }

    fn apply_connack_properties(&mut self, connack: &v5_0::Connack) -> Result<(), BrokerFailure> {
        for property in &connack.props {
            match property {
                Property::MaximumPacketSize(limit) => {
                    let max_bytes = limit.val();
                    if max_bytes == 0 {
                        return Err(protocol_settings_failure());
                    }
                    self.max_packet_size_send = Some(max_bytes);
                }
                Property::ServerKeepAlive(keep_alive) => {
                    let seconds = keep_alive.val();
                    if seconds == 0 {
                        self.keep_alive_ms = None;
                    } else {
                        let interval_ms = u64::from(seconds) * 1_000;
                        self.keep_alive_ms = Some(interval_ms);
                        self.pingresp_timeout_ms =
                            self.pingresp_timeout_ms.min((interval_ms / 2).max(1));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn handle_connected_packet(
        &mut self,
        packet: IncomingPacket,
        now: Instant,
        actions: &mut Vec<ClientAction>,
    ) -> Result<(), BrokerFailure> {
        match packet {
            IncomingPacket::Connack(_) => Err(protocol_settings_failure()),
            IncomingPacket::Disconnect(disconnect) => {
                self.phase = ClientPhase::Disconnected;
                Err(disconnect_failure(disconnect.reason_code()))
            }
            IncomingPacket::Pingresp => {
                self.pingresp_recv_at = None;
                Ok(())
            }
            IncomingPacket::Puback(ack) => self.handle_puback(&ack, actions),
            IncomingPacket::Pubrec(rec) => self.handle_pubrec(&rec, now, actions),
            IncomingPacket::Pubcomp(comp) => self.handle_pubcomp(&comp, actions),
            IncomingPacket::Publish(publish) => self.handle_server_publish(&publish, now, actions),
            IncomingPacket::Pubrel(rel) => self.handle_server_pubrel(&rel, now, actions),
        }
    }

    fn handle_puback(
        &mut self,
        ack: &v5_0::Puback,
        actions: &mut Vec<ClientAction>,
    ) -> Result<(), BrokerFailure> {
        let Some(InflightPublish::Puback { packet_id }) = self.inflight else {
            return Ok(());
        };
        if ack.packet_id() != packet_id {
            return Ok(());
        }
        self.inflight = None;
        match ack.reason_code() {
            None | Some(PubackReasonCode::Success | PubackReasonCode::NoMatchingSubscribers) => {
                actions.push(ClientAction::PublishComplete);
                Ok(())
            }
            Some(_) => Err(publication_rejected()),
        }
    }

    fn handle_pubrec(
        &mut self,
        rec: &v5_0::Pubrec,
        now: Instant,
        actions: &mut Vec<ClientAction>,
    ) -> Result<(), BrokerFailure> {
        let Some(InflightPublish::Pubrec { packet_id }) = self.inflight else {
            return Ok(());
        };
        if rec.packet_id() != packet_id {
            return Ok(());
        }
        if rec.reason_code().is_some_and(|code| code.is_failure()) {
            self.inflight = None;
            return Err(publication_rejected());
        }
        // MQTT 5.0 section 4.3.3 requires PUBREL for every non-error PUBREC (< 0x80), including 0x10.
        self.inflight = Some(InflightPublish::Pubcomp { packet_id });
        let pubrel = v5_0::Pubrel::builder()
            .packet_id(packet_id)
            .build()
            .map_err(|_| protocol_settings_failure())?;
        let wire = pubrel.to_continuous_buffer();
        self.check_outbound_size(&wire)?;
        self.note_packet_sent(now);
        actions.push(ClientAction::Send(wire));
        Ok(())
    }

    fn handle_pubcomp(
        &mut self,
        comp: &v5_0::Pubcomp,
        actions: &mut Vec<ClientAction>,
    ) -> Result<(), BrokerFailure> {
        let Some(InflightPublish::Pubcomp { packet_id }) = self.inflight else {
            return Ok(());
        };
        if comp.packet_id() != packet_id {
            return Ok(());
        }
        self.inflight = None;
        match comp.reason_code() {
            None | Some(PubcompReasonCode::Success) => {
                actions.push(ClientAction::PublishComplete);
                Ok(())
            }
            Some(PubcompReasonCode::PacketIdentifierNotFound) => Err(publication_rejected()),
        }
    }

    fn handle_server_publish(
        &mut self,
        publish: &v5_0::Publish,
        now: Instant,
        actions: &mut Vec<ClientAction>,
    ) -> Result<(), BrokerFailure> {
        match (publish.qos(), publish.packet_id()) {
            (Qos::AtMostOnce, _) => Ok(()),
            (Qos::AtLeastOnce, Some(packet_id)) => {
                let puback = v5_0::Puback::builder()
                    .packet_id(packet_id)
                    .build()
                    .map_err(|_| protocol_settings_failure())?;
                let wire = puback.to_continuous_buffer();
                self.check_outbound_size(&wire)?;
                self.note_packet_sent(now);
                actions.push(ClientAction::Send(wire));
                Ok(())
            }
            (Qos::ExactlyOnce, Some(packet_id)) => {
                let pubrec = v5_0::Pubrec::builder()
                    .packet_id(packet_id)
                    .build()
                    .map_err(|_| protocol_settings_failure())?;
                let wire = pubrec.to_continuous_buffer();
                self.check_outbound_size(&wire)?;
                self.note_packet_sent(now);
                actions.push(ClientAction::Send(wire));
                Ok(())
            }
            _ => Err(protocol_settings_failure()),
        }
    }

    fn handle_server_pubrel(
        &mut self,
        rel: &v5_0::Pubrel,
        now: Instant,
        actions: &mut Vec<ClientAction>,
    ) -> Result<(), BrokerFailure> {
        let pubcomp = v5_0::Pubcomp::builder()
            .packet_id(rel.packet_id())
            .build()
            .map_err(|_| protocol_settings_failure())?;
        let wire = pubcomp.to_continuous_buffer();
        self.check_outbound_size(&wire)?;
        self.note_packet_sent(now);
        actions.push(ClientAction::Send(wire));
        Ok(())
    }

    fn allocate_packet_id(&mut self) -> u16 {
        let id = self.next_packet_id;
        debug_assert!(id != 0, "MQTT packet identifiers must be non-zero");
        self.next_packet_id = if id == u16::MAX { 1 } else { id + 1 };
        id
    }

    fn note_packet_sent(&mut self, now: Instant) {
        if self.phase == ClientPhase::Connected {
            self.pingreq_send_at = self.keep_alive_ms.map(|ms| now + Duration::from_millis(ms));
        }
    }

    fn check_outbound_size(&self, wire: &[u8]) -> Result<(), BrokerFailure> {
        if self
            .max_packet_size_send
            .is_some_and(|limit| wire.len() > limit as usize)
        {
            return Err(protocol_settings_failure());
        }
        Ok(())
    }
}

fn parse_incoming_frame(frame: RawFrame) -> Result<IncomingPacket, BrokerFailure> {
    let packet_type = frame.first_byte >> 4;
    let flags = frame.first_byte & 0x0F;
    let body = frame.body;
    match packet_type {
        2 => {
            if flags != 0 || body.first().is_some_and(|ack_flags| ack_flags & !0x01 != 0) {
                return Err(protocol_settings_failure());
            }
            let (packet, consumed) = v5_0::Connack::parse(&body).map_err(mqtt_error_failure)?;
            ensure_full_consumption(consumed, body.len())?;
            Ok(IncomingPacket::Connack(packet))
        }
        3 => parse_publish_frame(flags, &body),
        4 => {
            if flags != 0 {
                return Err(protocol_settings_failure());
            }
            let (packet, consumed) = v5_0::Puback::parse(&body).map_err(mqtt_error_failure)?;
            ensure_full_consumption(consumed, body.len())?;
            Ok(IncomingPacket::Puback(packet))
        }
        5 => {
            if flags != 0 {
                return Err(protocol_settings_failure());
            }
            let (packet, consumed) = v5_0::Pubrec::parse(&body).map_err(mqtt_error_failure)?;
            ensure_full_consumption(consumed, body.len())?;
            Ok(IncomingPacket::Pubrec(packet))
        }
        6 => {
            if flags != 0x02 {
                return Err(protocol_settings_failure());
            }
            let (packet, consumed) = v5_0::Pubrel::parse(&body).map_err(mqtt_error_failure)?;
            ensure_full_consumption(consumed, body.len())?;
            Ok(IncomingPacket::Pubrel(packet))
        }
        7 => {
            if flags != 0 {
                return Err(protocol_settings_failure());
            }
            let (packet, consumed) = v5_0::Pubcomp::parse(&body).map_err(mqtt_error_failure)?;
            ensure_full_consumption(consumed, body.len())?;
            Ok(IncomingPacket::Pubcomp(packet))
        }
        13 => {
            if flags != 0 || !body.is_empty() {
                return Err(protocol_settings_failure());
            }
            let (_, consumed) = v5_0::Pingresp::parse(&body).map_err(mqtt_error_failure)?;
            ensure_full_consumption(consumed, body.len())?;
            Ok(IncomingPacket::Pingresp)
        }
        14 => {
            if flags != 0 {
                return Err(protocol_settings_failure());
            }
            let (packet, consumed) = v5_0::Disconnect::parse(&body).map_err(mqtt_error_failure)?;
            ensure_full_consumption(consumed, body.len())?;
            Ok(IncomingPacket::Disconnect(packet))
        }
        _ => Err(protocol_settings_failure()),
    }
}

fn parse_publish_frame(flags: u8, body: &[u8]) -> Result<IncomingPacket, BrokerFailure> {
    let qos_bits = (flags >> 1) & 0x03;
    let dup = (flags & 0x08) != 0;
    if qos_bits == 0 && dup {
        return Err(protocol_settings_failure());
    }
    let data: Arc<[u8]> = Arc::from(body);
    let (publish, consumed) = v5_0::Publish::parse(flags, data).map_err(mqtt_error_failure)?;
    ensure_full_consumption(consumed, body.len())?;
    if publish.qos() != Qos::AtMostOnce && publish.packet_id().is_none_or(|id| id == 0) {
        return Err(protocol_settings_failure());
    }
    Ok(IncomingPacket::Publish(publish))
}

fn ensure_full_consumption(consumed: usize, body_len: usize) -> Result<(), BrokerFailure> {
    if consumed != body_len {
        return Err(protocol_settings_failure());
    }
    Ok(())
}

pub(super) fn build_connect_packet(
    options: &BrokerOptions,
    session_expiry_secs: u32,
) -> Result<v5_0::Connect, BrokerFailure> {
    let props = vec![
        Property::SessionExpiryInterval(
            SessionExpiryInterval::new(session_expiry_secs)
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
            CorrelationData::new(options.forwarder_id.as_str())
                .map_err(|_| protocol_settings_failure())?,
        ),
        Property::UserProperty(
            UserProperty::new("schema-version", "1").map_err(|_| protocol_settings_failure())?,
        ),
    ];

    let mut builder = v5_0::Connect::builder()
        .client_id(options.client_id.as_str())
        .map_err(|_| protocol_settings_failure())?
        .clean_start(false)
        .keep_alive(options.keep_alive_secs)
        .props(props)
        .will_message(
            options.will_topic.as_str(),
            &options.will_payload,
            options.will_qos,
            options.will_retain,
        )
        .map_err(|_| protocol_settings_failure())?
        .will_props(will_props);
    if let Some(user) = &options.username {
        builder = builder
            .user_name(user.as_str())
            .map_err(|_| protocol_settings_failure())?;
        if let Some(pass) = &options.password {
            builder = builder
                .password(pass.as_str())
                .map_err(|_| protocol_settings_failure())?;
        }
    }
    builder.build().map_err(|_| protocol_settings_failure())
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

pub(super) const fn qos(value: u8) -> Qos {
    match value {
        0 => Qos::AtMostOnce,
        2 => Qos::ExactlyOnce,
        _ => Qos::AtLeastOnce,
    }
}
