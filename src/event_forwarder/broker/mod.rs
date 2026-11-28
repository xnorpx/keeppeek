mod protocol;
mod transport;

#[cfg(test)]
mod tests;

use self::{
    protocol::{ClientAction, MqttClientState, TimerAction, build_connect_packet, qos},
    transport::{BrokerTransport, is_timeout_error, read_ca_file, socket_timeout},
};
use super::{
    BrokerFailure, BrokerFailureKind,
    config::MqttForwarderConfig,
    model::{Publication, status_topic},
};
use mqtt_protocol_core::mqtt::{
    packet::{Qos, v5_0},
    result_code::{ConnectReasonCode, DisconnectReasonCode, MqttError},
};
use std::{
    fmt,
    io::{self, Read, Write},
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
/// Sixteen MiB matches the previous forwarder ceiling and bounds receive allocations.
const MQTT_MAX_PACKET_BYTES: u32 = 16 * 1_024 * 1_024;

/// MQTT 5 session expiry interval in seconds requested during CONNECT.
///
/// Twenty-four hours lets a reconnecting broker resume session state after brief outages.
const MQTT_SESSION_EXPIRY_SECS: u32 = 24 * 60 * 60;

/// Reusable socket read buffer size in bytes for MQTT control and ack frames.
const READ_BUFFER_BYTES: usize = 8 * 1_024;

/// Maximum write wait in milliseconds for best-effort DISCONNECT on keep-alive timeout.
const KEEP_ALIVE_DISCONNECT_TIMEOUT_MS: u64 = 250;

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
        let port = url.port().unwrap_or(if use_tls { 8883 } else { 1883 });
        let tls_ca_pem = if use_tls {
            config
                .tls_ca_path
                .as_deref()
                .map(read_ca_file)
                .transpose()?
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

    fn connect_packet(&self) -> Result<v5_0::Connect, BrokerFailure> {
        build_connect_packet(self, MQTT_SESSION_EXPIRY_SECS)
    }

    fn initial_keep_alive_ms(&self) -> Option<u64> {
        self.pingreq_interval_override_ms
            .or_else(|| (self.keep_alive_secs > 0).then(|| u64::from(self.keep_alive_secs) * 1_000))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitTarget {
    Connected,
    PublishComplete,
    IdlePoll,
}

struct ActiveSession {
    transport: BrokerTransport,
    state: MqttClientState,
    read_buffer: Vec<u8>,
}

impl ActiveSession {
    fn open(options: &BrokerOptions, deadline: Instant) -> Result<Self, BrokerFailure> {
        let transport = BrokerTransport::connect(options, deadline)?;
        let state =
            MqttClientState::new(options.initial_keep_alive_ms(), options.pingresp_timeout_ms);
        let mut active = Self {
            transport,
            state,
            read_buffer: vec![0_u8; READ_BUFFER_BYTES],
        };
        let connect = options.connect_packet()?;
        let wire = active.state.start_connect(&connect)?;
        active.write_all_until(&wire, deadline)?;
        active.drive_until(deadline, WaitTarget::Connected)?;
        Ok(active)
    }

    fn publish(
        &mut self,
        publication: &Publication,
        deadline: Instant,
    ) -> Result<(), BrokerFailure> {
        let (wire, wait_for_ack) = self.state.start_publish(publication, Instant::now())?;
        self.write_all_until(&wire, deadline)?;
        if wait_for_ack {
            self.drive_until(deadline, WaitTarget::PublishComplete)?;
        }
        Ok(())
    }

    fn poll(&mut self, deadline: Instant) -> Result<(), BrokerFailure> {
        self.drive_until(deadline, WaitTarget::IdlePoll)
    }

    fn disconnect(mut self, deadline: Instant) -> Result<(), BrokerFailure> {
        let wire = self.state.start_disconnect()?;
        let result = self.write_all_until(&wire, deadline);
        self.transport.shutdown(deadline, result.is_ok());
        result
    }

    fn drive_until(&mut self, deadline: Instant, target: WaitTarget) -> Result<(), BrokerFailure> {
        let timeout_is_ok = target == WaitTarget::IdlePoll;
        loop {
            let now = Instant::now();
            if !timeout_is_ok && now >= deadline {
                return Err(timeout_failure());
            }
            let write_deadline = self.write_deadline(now, deadline, timeout_is_ok);
            self.fire_due_timers(now, write_deadline)?;
            let now = Instant::now();
            if now >= deadline {
                return if timeout_is_ok {
                    Ok(())
                } else {
                    Err(timeout_failure())
                };
            }
            let wait_until = self
                .state
                .next_timer_deadline()
                .map_or(deadline, |timer| timer.min(deadline));
            if wait_until <= now {
                continue;
            }
            let wait = wait_until
                .saturating_duration_since(now)
                .max(Duration::from_millis(1));
            self.transport
                .set_read_timeout(Some(wait))
                .map_err(|_| network_unavailable())?;
            match self.transport.read(&mut self.read_buffer) {
                Ok(0) => return Err(network_unavailable()),
                Ok(bytes_read) => {
                    let action_deadline =
                        self.write_deadline(Instant::now(), deadline, timeout_is_ok);
                    let actions = self
                        .state
                        .feed_bytes(&self.read_buffer[..bytes_read], Instant::now())?;
                    if self.apply_actions(actions, target, action_deadline)? {
                        return Ok(());
                    }
                }
                Err(error)
                    if error.kind() == io::ErrorKind::Interrupted || is_timeout_error(&error) => {}
                Err(_) => return Err(network_unavailable()),
            }
        }
    }

    fn write_deadline(&self, now: Instant, deadline: Instant, timeout_is_ok: bool) -> Instant {
        if timeout_is_ok {
            deadline.max(now + Duration::from_millis(self.state.pingresp_timeout_ms()))
        } else {
            deadline
        }
    }

    fn fire_due_timers(
        &mut self,
        now: Instant,
        write_deadline: Instant,
    ) -> Result<(), BrokerFailure> {
        match self.state.fire_due_timers(now) {
            TimerAction::None => Ok(()),
            TimerAction::SendPingreq(wire) => self.write_all_until(&wire, write_deadline),
            TimerAction::KeepAliveExpired(disconnect_wire) => {
                let disconnect_deadline = (now
                    + Duration::from_millis(KEEP_ALIVE_DISCONNECT_TIMEOUT_MS))
                .min(write_deadline);
                let _ = self.write_all_until(&disconnect_wire, disconnect_deadline);
                Err(timeout_failure())
            }
        }
    }

    fn apply_actions(
        &mut self,
        actions: Vec<ClientAction>,
        target: WaitTarget,
        deadline: Instant,
    ) -> Result<bool, BrokerFailure> {
        let mut target_reached = false;
        for action in actions {
            match action {
                ClientAction::Send(wire) => self.write_all_until(&wire, deadline)?,
                ClientAction::Connected if target == WaitTarget::Connected => {
                    target_reached = true;
                }
                ClientAction::PublishComplete if target == WaitTarget::PublishComplete => {
                    target_reached = true;
                }
                ClientAction::Connected | ClientAction::PublishComplete => {}
            }
        }
        Ok(target_reached)
    }

    fn write_all_until(
        &mut self,
        mut bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), BrokerFailure> {
        while !bytes.is_empty() {
            let timeout = socket_timeout(deadline)?;
            self.transport
                .set_write_timeout(Some(timeout))
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
        loop {
            let timeout = socket_timeout(deadline)?;
            self.transport
                .set_write_timeout(Some(timeout))
                .map_err(|_| network_unavailable())?;
            match self.transport.flush() {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if is_timeout_error(&error) => {
                    if Instant::now() >= deadline {
                        return Err(timeout_failure());
                    }
                }
                Err(_) => return Err(network_unavailable()),
            }
        }
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

fn ca_read_failure() -> BrokerFailure {
    BrokerFailure {
        kind: BrokerFailureKind::Tls,
        detail: "Configured MQTT CA certificate could not be read.".to_owned(),
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
