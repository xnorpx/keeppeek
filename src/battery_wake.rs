//! Wakes registered Reolink battery cameras on the local network.

use crate::{config::BatteryWakeConfig, shutdown::Shutdown};
use quick_xml::{
    Reader, Writer,
    events::{BytesEnd, BytesStart, BytesText, Event},
};
use reo_proto::{BcUdpPacket, UdpDiscovery};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Limits discovery reads to control-sized packets rather than media payloads.
const MAX_DISCOVERY_DATAGRAM: usize = 4 * 1024;
/// Bounds camera-supplied identifiers before they reach the registration map.
const MAX_UID_LEN: usize = 64;
/// Bounds the registry to configured cameras instead of arbitrary LAN traffic.
const MAX_REGISTRATIONS: usize = 256;
/// Matches the camera wake cadence expected by Reolink P2P firmware.
const WAKE_BURST_COUNT: usize = 10;
/// Spaces wake packets so sleeping firmware has repeated opportunities to receive one.
const WAKE_BURST_INTERVAL: Duration = Duration::from_millis(100);
/// Lets worker shutdown interrupt listener waits promptly without polling hot.
const RECEIVE_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Bounds the pre-discovery wait for the local wake service acknowledgement.
const WAKE_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct BatteryWakeHandle {
    core: Arc<WakeCore>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BatteryWakeHealth {
    pub registered: bool,
    pub last_seen_age_ms: Option<u64>,
    pub wake_pending_age_ms: Option<u64>,
    pub sleeping: bool,
}

impl BatteryWakeHandle {
    /// Requests a local wake burst before direct BCUDP discovery.
    ///
    /// Returns `Ok(true)` when the local server found a registered camera,
    /// `Ok(false)` when the camera is unavailable, and an error for local I/O
    /// or malformed wake replies.
    pub fn request_wake(
        &self,
        socket: &UdpSocket,
        camera_ip: IpAddr,
        uid: &str,
        client_id: i32,
        transmission_id: u32,
    ) -> anyhow::Result<bool> {
        let server = self.server_address(camera_ip)?;
        let client = SocketAddr::new(
            IpAddr::V4(local_route_ip(camera_ip)?),
            socket.local_addr()?.port(),
        );
        let packet = client_connect_packet(transmission_id, uid, client, client_id)?;
        socket.send_to(&packet, server)?;

        let deadline = Instant::now() + WAKE_REPLY_TIMEOUT;
        let mut buffer = [0u8; MAX_DISCOVERY_DATAGRAM];
        while Instant::now() < deadline {
            let timeout = deadline
                .saturating_duration_since(Instant::now())
                .min(RECEIVE_POLL_INTERVAL)
                .max(Duration::from_millis(1));
            socket.set_read_timeout(Some(timeout))?;
            match socket.recv_from(&mut buffer) {
                Ok((read, _)) => {
                    let Ok((_, message)) = decode_message(&buffer[..read]) else {
                        continue;
                    };
                    if let WakeMessage::ClientConnectReply { response } = message {
                        let accepted = response == 0;
                        if accepted {
                            self.core.note_wake_requested(uid);
                        }
                        return Ok(accepted);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }

    pub(crate) fn health(&self, uid: &str) -> BatteryWakeHealth {
        self.health_at(uid, Instant::now())
    }

    pub(crate) fn note_media_connected(&self, uid: &str) {
        self.core.note_media_connected(uid);
    }

    pub(crate) fn note_media_disconnected(&self, uid: &str) {
        self.core.note_wake_requested(uid);
    }

    fn health_at(&self, uid: &str, now: Instant) -> BatteryWakeHealth {
        let registration = self.core.registration_at(uid, now);
        let wake_pending = self.core.wake_pending(uid);
        BatteryWakeHealth {
            registered: registration.is_some(),
            last_seen_age_ms: registration.map(|registration| {
                now.saturating_duration_since(registration.last_seen)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX)
            }),
            wake_pending_age_ms: wake_pending.map(|requested_at| {
                now.saturating_duration_since(requested_at)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX)
            }),
            sleeping: registration.is_some() && wake_pending.is_none(),
        }
    }

    fn server_address(&self, camera_ip: IpAddr) -> anyhow::Result<SocketAddr> {
        let bind = self
            .core
            .config
            .bind
            .filter(|bind| !bind.is_unspecified())
            .unwrap_or(local_route_ip(camera_ip)?);
        Ok(SocketAddr::new(
            IpAddr::V4(bind),
            self.core.config.register_port,
        ))
    }
}

#[derive(Debug)]
pub struct BatteryWakeService {
    handle: BatteryWakeHandle,
    workers: Vec<JoinHandle<()>>,
}

impl BatteryWakeService {
    /// Starts opt-in P2P registration listeners for configured battery camera UIDs.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration is invalid or either UDP listener cannot bind.
    pub fn start(
        config: BatteryWakeConfig,
        camera_uids: impl IntoIterator<Item = String>,
        shutdown: Shutdown,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        let bind = config.bind.unwrap_or(Ipv4Addr::UNSPECIFIED);
        let middleman = UdpSocket::bind(SocketAddr::new(IpAddr::V4(bind), config.middleman_port))?;
        let register = UdpSocket::bind(SocketAddr::new(IpAddr::V4(bind), config.register_port))?;
        middleman.set_read_timeout(Some(RECEIVE_POLL_INTERVAL))?;
        register.set_read_timeout(Some(RECEIVE_POLL_INTERVAL))?;

        let allowed_uids = camera_uids
            .into_iter()
            .map(|uid| uid.trim().to_owned())
            .filter(|uid| valid_uid(uid))
            .collect();
        let core = Arc::new(WakeCore::new(config, allowed_uids));
        let register_socket = Arc::new(register);
        let middleman_core = core.clone();
        let middleman_shutdown = shutdown.clone();
        let middleman_worker = thread::Builder::new()
            .name("battery-wake-middleman".to_owned())
            .spawn(move || run_middleman(middleman, middleman_core, middleman_shutdown))?;

        let register_core = core.clone();
        let register_shutdown = shutdown;
        let register_worker = thread::Builder::new()
            .name("battery-wake-register".to_owned())
            .spawn(move || run_register(register_socket, register_core, register_shutdown))?;

        Ok(Self {
            handle: BatteryWakeHandle { core },
            workers: vec![middleman_worker, register_worker],
        })
    }

    /// Returns the handle supplied to BCUDP camera workers.
    pub fn handle(&self) -> BatteryWakeHandle {
        self.handle.clone()
    }

    /// Waits for the listener workers after shared shutdown is requested.
    pub fn join(self) {
        for worker in self.workers {
            if worker.join().is_err() {
                tracing::warn!("battery wake worker panicked");
            }
        }
    }
}

#[derive(Debug)]
struct WakeCore {
    config: BatteryWakeConfig,
    allowed_uids: HashSet<String>,
    state: Mutex<WakeState>,
}

#[derive(Debug, Default)]
struct WakeState {
    anchors: HashMap<String, SessionAnchor>,
    registrations: HashMap<String, Registration>,
    pending_wakes: HashMap<String, Instant>,
}

#[derive(Debug, Clone, Copy)]
struct SessionAnchor {
    token: u64,
    access_code: u32,
    issued_at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct Registration {
    address: SocketAddr,
    last_seen: Instant,
}

impl WakeCore {
    fn new(config: BatteryWakeConfig, allowed_uids: HashSet<String>) -> Self {
        Self {
            config,
            allowed_uids,
            state: Mutex::new(WakeState::default()),
        }
    }

    fn configured_uid(&self, reported_uid: &str) -> Option<String> {
        self.allowed_uids
            .iter()
            .filter(|configured| {
                reported_uid == configured.as_str() || reported_uid.starts_with(configured.as_str())
            })
            .max_by_key(|configured| configured.len())
            .cloned()
    }

    fn advertise_address(&self, peer: SocketAddr) -> anyhow::Result<SocketAddr> {
        let bind = self
            .config
            .bind
            .filter(|bind| !bind.is_unspecified())
            .unwrap_or(local_route_ip(peer.ip())?);
        Ok(SocketAddr::new(IpAddr::V4(bind), self.config.register_port))
    }

    fn issue_anchor(&self, uid: &str) -> Option<SessionAnchor> {
        let configured_uid = self.configured_uid(uid)?;
        let anchor = SessionAnchor {
            token: rand::random(),
            access_code: rand::random(),
            issued_at: Instant::now(),
        };
        let mut state = self.state.lock().expect("battery wake state poisoned");
        state.anchors.insert(configured_uid, anchor);
        Some(anchor)
    }

    fn register(&self, uid: &str, address: SocketAddr) -> bool {
        self.register_at(uid, address, Instant::now())
    }

    fn register_at(&self, uid: &str, address: SocketAddr, now: Instant) -> bool {
        let Some(configured_uid) = self.configured_uid(uid) else {
            return false;
        };
        let mut state = self.state.lock().expect("battery wake state poisoned");
        if state.registrations.len() >= MAX_REGISTRATIONS
            && !state.registrations.contains_key(&configured_uid)
        {
            return false;
        }
        let returned_to_sleep =
            state
                .pending_wakes
                .get(&configured_uid)
                .is_some_and(|pending_since| {
                    now.saturating_duration_since(*pending_since)
                        >= Duration::from_secs(self.config.heartbeat_secs)
                });
        if returned_to_sleep {
            state.pending_wakes.remove(&configured_uid);
        }
        state.registrations.insert(
            configured_uid,
            Registration {
                address,
                last_seen: now,
            },
        );
        true
    }

    fn anchor(&self, uid: &str, token: u64) -> Option<SessionAnchor> {
        let configured_uid = self.configured_uid(uid)?;
        let stale_after = Duration::from_secs(self.config.stale_after_secs);
        let state = self.state.lock().expect("battery wake state poisoned");
        state
            .anchors
            .get(&configured_uid)
            .filter(|anchor| anchor.token == token && anchor.issued_at.elapsed() <= stale_after)
            .copied()
    }

    fn registration(&self, uid: &str) -> Option<Registration> {
        self.registration_at(uid, Instant::now())
    }

    fn registration_at(&self, uid: &str, now: Instant) -> Option<Registration> {
        let configured_uid = self.configured_uid(uid)?;
        let stale_after = Duration::from_secs(self.config.stale_after_secs);
        let mut state = self.state.lock().expect("battery wake state poisoned");
        let registration = state.registrations.get(&configured_uid).copied()?;
        if now.saturating_duration_since(registration.last_seen) > stale_after {
            state.registrations.remove(&configured_uid);
            state.pending_wakes.remove(&configured_uid);
            return None;
        }
        Some(registration)
    }

    fn note_wake_requested(&self, uid: &str) {
        let Some(configured_uid) = self.configured_uid(uid) else {
            return;
        };
        self.state
            .lock()
            .expect("battery wake state poisoned")
            .pending_wakes
            .insert(configured_uid, Instant::now());
    }

    fn note_media_connected(&self, uid: &str) {
        let Some(configured_uid) = self.configured_uid(uid) else {
            return;
        };
        self.state
            .lock()
            .expect("battery wake state poisoned")
            .pending_wakes
            .remove(&configured_uid);
    }

    fn wake_pending(&self, uid: &str) -> Option<Instant> {
        let configured_uid = self.configured_uid(uid)?;
        self.state
            .lock()
            .expect("battery wake state poisoned")
            .pending_wakes
            .get(&configured_uid)
            .copied()
    }
}

fn run_middleman(socket: UdpSocket, core: Arc<WakeCore>, shutdown: Shutdown) {
    let mut buffer = [0u8; MAX_DISCOVERY_DATAGRAM];
    while !shutdown.is_cancelled() {
        match socket.recv_from(&mut buffer) {
            Ok((read, source)) => match handle_middleman(&core, source, &buffer[..read]) {
                Ok(Some(reply)) => {
                    if let Err(error) = socket.send_to(&reply, source) {
                        tracing::warn!(%source, %error, "unable to reply to battery camera discovery");
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(%source, %error, "ignoring invalid battery camera discovery");
                }
            },
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => tracing::warn!(%error, "battery wake middleman receive failed"),
        }
    }
}

fn run_register(socket: Arc<UdpSocket>, core: Arc<WakeCore>, shutdown: Shutdown) {
    let mut buffer = [0u8; MAX_DISCOVERY_DATAGRAM];
    while !shutdown.is_cancelled() {
        match socket.recv_from(&mut buffer) {
            Ok((read, source)) => {
                if let Err(error) = handle_register(&socket, &core, source, &buffer[..read]) {
                    tracing::debug!(%source, %error, "ignoring invalid battery camera registration");
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => tracing::warn!(%error, "battery wake register receive failed"),
        }
    }
}

fn handle_middleman(
    core: &WakeCore,
    source: SocketAddr,
    datagram: &[u8],
) -> anyhow::Result<Option<Vec<u8>>> {
    let (transmission_id, message) = decode_message(datagram)?;
    let WakeMessage::DeviceMiddlemanQuery { uid } = message else {
        return Ok(None);
    };
    let Some(anchor) = core.issue_anchor(&uid) else {
        return Ok(None);
    };
    let register = core.advertise_address(source)?;
    middleman_reply_packet(transmission_id, register, anchor).map(Some)
}

fn handle_register(
    socket: &UdpSocket,
    core: &WakeCore,
    source: SocketAddr,
    datagram: &[u8],
) -> anyhow::Result<()> {
    let (transmission_id, message) = decode_message(datagram)?;
    match message {
        WakeMessage::DeviceHeartbeat { uid, needs_reply } => {
            if !core.register(&uid, source) {
                return Ok(());
            }
            if needs_reply {
                let reply = heartbeat_reply_packet(
                    transmission_id,
                    Duration::from_secs(core.config.heartbeat_secs),
                )?;
                socket.send_to(&reply, source)?;
            }
        }
        WakeMessage::DeviceRegistration { uid, token } => {
            let Some(anchor) = core.anchor(&uid, token) else {
                return Ok(());
            };
            let reply = registration_reply_packet(transmission_id, anchor.access_code)?;
            socket.send_to(&reply, source)?;
        }
        WakeMessage::DeviceDisconnect => {
            let reply = disconnect_reply_packet(transmission_id)?;
            socket.send_to(&reply, source)?;
        }
        WakeMessage::ClientConnect {
            uid,
            client_id,
            client_port,
        } => {
            let Some(camera) = core.registration(&uid) else {
                let reply = client_connect_reply_packet(transmission_id, None, None, 0, -1)?;
                socket.send_to(&reply, source)?;
                return Ok(());
            };
            let client = SocketAddr::new(source.ip(), client_port);
            let relay = core.advertise_address(camera.address)?;
            let session_id = rand::random();
            for index in 0..WAKE_BURST_COUNT {
                let wake = wake_packet(transmission_id, client, relay, session_id, client_id)?;
                socket.send_to(&wake, camera.address)?;
                if index + 1 < WAKE_BURST_COUNT {
                    thread::sleep(WAKE_BURST_INTERVAL);
                }
            }
            let reply = client_connect_reply_packet(
                transmission_id,
                Some(camera.address),
                Some(relay),
                session_id,
                0,
            )?;
            socket.send_to(&reply, source)?;
            let ticket =
                client_ticket_packet(transmission_id, camera.address, session_id, client_id)?;
            socket.send_to(&ticket, source)?;
        }
        _ => {}
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum WakeMessage {
    DeviceMiddlemanQuery {
        uid: String,
    },
    DeviceHeartbeat {
        uid: String,
        needs_reply: bool,
    },
    DeviceRegistration {
        uid: String,
        token: u64,
    },
    DeviceDisconnect,
    ClientConnect {
        uid: String,
        client_id: i32,
        client_port: u16,
    },
    ClientConnectReply {
        response: i32,
    },
    Other,
}

fn decode_message(datagram: &[u8]) -> anyhow::Result<(u32, WakeMessage)> {
    let BcUdpPacket::Discovery(UdpDiscovery {
        transmission_id,
        xml,
    }) = BcUdpPacket::decode(datagram)?
    else {
        anyhow::bail!("battery wake packet is not discovery XML");
    };
    Ok((transmission_id, parse_message(&xml)?))
}

fn parse_message(xml: &[u8]) -> anyhow::Result<WakeMessage> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut operation = None;
    let mut field = None;
    let mut uid = None;
    let mut token = None;
    let mut needs_reply = false;
    let mut client_id = None;
    let mut client_port = None;
    let mut response = None;

    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let name = element.name().as_ref().to_owned();
                if name != "P2P" && operation.is_none() {
                    operation = Some(name);
                } else {
                    field = Some(name);
                }
            }
            Event::Text(text) => {
                parse_field(
                    field.as_deref(),
                    text.as_ref(),
                    &mut uid,
                    &mut token,
                    &mut needs_reply,
                    &mut client_id,
                    &mut client_port,
                    &mut response,
                );
            }
            Event::CData(text) => {
                parse_field(
                    field.as_deref(),
                    text.as_ref(),
                    &mut uid,
                    &mut token,
                    &mut needs_reply,
                    &mut client_id,
                    &mut client_port,
                    &mut response,
                );
            }
            Event::End(_) => field = None,
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    let message = match operation.as_deref() {
        Some("D2M_Q") => WakeMessage::DeviceMiddlemanQuery {
            uid: required_uid(uid)?,
        },
        Some("D2R_HB") => WakeMessage::DeviceHeartbeat {
            uid: required_uid(uid)?,
            needs_reply,
        },
        Some("D2R_R") => WakeMessage::DeviceRegistration {
            uid: required_uid(uid)?,
            token: token.ok_or_else(|| anyhow::anyhow!("battery registration is missing token"))?,
        },
        Some("D2R_DISC") => WakeMessage::DeviceDisconnect,
        Some("C2R_C") => WakeMessage::ClientConnect {
            uid: required_uid(uid)?,
            client_id: client_id
                .ok_or_else(|| anyhow::anyhow!("battery wake request is missing client id"))?,
            client_port: client_port
                .ok_or_else(|| anyhow::anyhow!("battery wake request is missing client port"))?,
        },
        Some("R2C_C_R") => WakeMessage::ClientConnectReply {
            response: response
                .ok_or_else(|| anyhow::anyhow!("battery wake response is missing status"))?,
        },
        _ => WakeMessage::Other,
    };
    Ok(message)
}

#[allow(clippy::too_many_arguments)]
fn parse_field(
    field: Option<&str>,
    value: &str,
    uid: &mut Option<String>,
    token: &mut Option<u64>,
    needs_reply: &mut bool,
    client_id: &mut Option<i32>,
    client_port: &mut Option<u16>,
    response: &mut Option<i32>,
) {
    match field {
        Some("uid") => *uid = Some(value.to_owned()),
        Some("token") => *token = value.parse().ok(),
        Some("needrsp") => *needs_reply = value == "1",
        Some("cid") => *client_id = value.parse().ok(),
        Some("port") => *client_port = value.parse().ok(),
        Some("rsp") => *response = value.parse().ok(),
        _ => {}
    }
}

fn required_uid(uid: Option<String>) -> anyhow::Result<String> {
    let uid = uid.ok_or_else(|| anyhow::anyhow!("battery wake packet is missing UID"))?;
    if !valid_uid(&uid) {
        anyhow::bail!("battery wake packet has invalid UID");
    }
    Ok(uid)
}

fn valid_uid(uid: &str) -> bool {
    !uid.is_empty()
        && uid.len() <= MAX_UID_LEN
        && uid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn local_route_ip(peer: IpAddr) -> anyhow::Result<Ipv4Addr> {
    let IpAddr::V4(peer) = peer else {
        anyhow::bail!("battery wake requires an IPv4 camera address");
    };
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    probe.connect(SocketAddr::new(IpAddr::V4(peer), 9))?;
    match probe.local_addr()?.ip() {
        IpAddr::V4(address) if !address.is_unspecified() => Ok(address),
        _ => anyhow::bail!("could not determine a local IPv4 route for battery wake"),
    }
}

fn middleman_reply_packet(
    transmission_id: u32,
    register: SocketAddr,
    anchor: SessionAnchor,
) -> anyhow::Result<Vec<u8>> {
    discovery_packet(transmission_id, "M2D_Q_R", |writer| {
        write_endpoint(writer, "reg", register)?;
        write_endpoint(writer, "log", register)?;
        write_empty(writer, "timer")?;
        write_empty(writer, "retry")?;
        write_text(writer, "rsp", "0")?;
        write_text(writer, "token", &anchor.token.to_string())?;
        write_text(writer, "ac", &anchor.access_code.to_string())
    })
}

fn heartbeat_reply_packet(transmission_id: u32, heartbeat: Duration) -> anyhow::Result<Vec<u8>> {
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    discovery_packet(transmission_id, "R2D_HB_R", |writer| {
        write_text(writer, "rsp", "0")?;
        write_text(writer, "time", &unix_seconds.to_string())?;
        write_start(writer, "timer")?;
        write_text(writer, "hb", &heartbeat.as_millis().to_string())?;
        write_end(writer, "timer")
    })
}

fn registration_reply_packet(transmission_id: u32, access_code: u32) -> anyhow::Result<Vec<u8>> {
    discovery_packet(transmission_id, "R2D_R_R", |writer| {
        write_text(writer, "rsp", "-4")?;
        write_text(writer, "ac", &access_code.to_string())
    })
}

fn disconnect_reply_packet(transmission_id: u32) -> anyhow::Result<Vec<u8>> {
    discovery_packet(transmission_id, "R2D_DC_R", |writer| {
        write_text(writer, "rsp", "0")
    })
}

fn client_connect_packet(
    transmission_id: u32,
    uid: &str,
    client: SocketAddr,
    client_id: i32,
) -> anyhow::Result<Vec<u8>> {
    discovery_packet(transmission_id, "C2R_C", |writer| {
        write_text(writer, "uid", uid)?;
        write_start(writer, "cli")?;
        write_text(writer, "ip", &client.ip().to_string())?;
        write_text(writer, "port", &client.port().to_string())?;
        write_end(writer, "cli")?;
        write_text(writer, "cid", &client_id.to_string())
    })
}

fn client_connect_reply_packet(
    transmission_id: u32,
    camera: Option<SocketAddr>,
    relay: Option<SocketAddr>,
    session_id: u32,
    response: i32,
) -> anyhow::Result<Vec<u8>> {
    discovery_packet(transmission_id, "R2C_C_R", |writer| {
        if let Some(camera) = camera {
            write_endpoint(writer, "dev", camera)?;
            write_endpoint(writer, "dmap", camera)?;
        }
        if let Some(relay) = relay {
            write_endpoint(writer, "relay", relay)?;
        }
        write_text(writer, "nat", "NULL")?;
        if response == 0 {
            write_text(writer, "sid", &session_id.to_string())?;
        }
        write_text(writer, "rsp", &response.to_string())?;
        write_text(writer, "ac", "0")
    })
}

fn client_ticket_packet(
    transmission_id: u32,
    camera: SocketAddr,
    session_id: u32,
    client_id: i32,
) -> anyhow::Result<Vec<u8>> {
    discovery_packet(transmission_id, "R2C_T", |writer| {
        write_endpoint(writer, "dev", camera)?;
        write_endpoint(writer, "dmap", camera)?;
        write_text(writer, "sid", &session_id.to_string())?;
        write_text(writer, "cid", &client_id.to_string())
    })
}

fn wake_packet(
    transmission_id: u32,
    client: SocketAddr,
    relay: SocketAddr,
    session_id: u32,
    client_id: i32,
) -> anyhow::Result<Vec<u8>> {
    discovery_packet(transmission_id, "R2D_C", |writer| {
        write_endpoint(writer, "cli", client)?;
        write_endpoint(writer, "cmap", client)?;
        write_endpoint(writer, "relay", relay)?;
        write_text(writer, "sid", &session_id.to_string())?;
        write_text(writer, "cid", &client_id.to_string())
    })
}

fn discovery_packet(
    transmission_id: u32,
    operation: &str,
    write_operation: impl FnOnce(&mut Writer<Vec<u8>>) -> anyhow::Result<()>,
) -> anyhow::Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::new());
    write_start(&mut writer, "P2P")?;
    write_start(&mut writer, operation)?;
    write_operation(&mut writer)?;
    write_end(&mut writer, operation)?;
    write_end(&mut writer, "P2P")?;
    Ok(BcUdpPacket::Discovery(UdpDiscovery {
        transmission_id,
        xml: writer.into_inner(),
    })
    .encode()?)
}

fn write_endpoint(
    writer: &mut Writer<Vec<u8>>,
    name: &str,
    address: SocketAddr,
) -> anyhow::Result<()> {
    write_start(writer, name)?;
    write_text(writer, "ip", &address.ip().to_string())?;
    write_text(writer, "port", &address.port().to_string())?;
    write_end(writer, name)
}

fn write_empty(writer: &mut Writer<Vec<u8>>, name: &str) -> anyhow::Result<()> {
    writer.write_event(Event::Empty(BytesStart::new(name)))?;
    Ok(())
}

fn write_start(writer: &mut Writer<Vec<u8>>, name: &str) -> anyhow::Result<()> {
    writer.write_event(Event::Start(BytesStart::new(name)))?;
    Ok(())
}

fn write_end(writer: &mut Writer<Vec<u8>>, name: &str) -> anyhow::Result<()> {
    writer.write_event(Event::End(BytesEnd::new(name)))?;
    Ok(())
}

fn write_text(writer: &mut Writer<Vec<u8>>, name: &str, value: &str) -> anyhow::Result<()> {
    write_start(writer, name)?;
    writer.write_event(Event::Text(BytesText::new(value)))?;
    write_end(writer, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BatteryWakeConfig {
        BatteryWakeConfig {
            enabled: true,
            bind: Some(Ipv4Addr::LOCALHOST),
            middleman_port: 9_999,
            register_port: 58_200,
            heartbeat_secs: 20,
            stale_after_secs: 80,
        }
    }

    fn core() -> WakeCore {
        WakeCore::new(config(), HashSet::from(["BATTERYCAMERA0001".to_owned()]))
    }

    fn packet(transmission_id: u32, xml: &str) -> Vec<u8> {
        BcUdpPacket::Discovery(UdpDiscovery {
            transmission_id,
            xml: xml.as_bytes().to_vec(),
        })
        .encode()
        .unwrap()
    }

    #[test]
    fn middleman_registers_only_configured_uid() {
        let core = core();
        let source: SocketAddr = "127.0.0.1:60000".parse().unwrap();
        let known = packet(
            7,
            "<P2P><D2M_Q><uid>BATTERYCAMERA0001ABCD</uid></D2M_Q></P2P>",
        );
        let reply = handle_middleman(&core, source, &known).unwrap().unwrap();
        let BcUdpPacket::Discovery(reply) = BcUdpPacket::decode(&reply).unwrap() else {
            panic!("expected discovery reply");
        };
        let xml = std::str::from_utf8(&reply.xml).unwrap();
        assert!(xml.contains("<M2D_Q_R>"));
        assert!(xml.contains("<port>58200</port>"));
        assert!(xml.contains("<token>"));

        let unknown = packet(8, "<P2P><D2M_Q><uid>UNKNOWN0000000000</uid></D2M_Q></P2P>");
        assert!(handle_middleman(&core, source, &unknown).unwrap().is_none());
    }

    #[test]
    fn heartbeat_accepts_configured_uid_suffix_and_expires() {
        let core = core();
        let source: SocketAddr = "192.168.1.20:50000".parse().unwrap();
        assert!(core.register("BATTERYCAMERA0001ABCD", source));
        assert_eq!(
            core.registration("BATTERYCAMERA0001").unwrap().address,
            source
        );
        assert!(!core.register("UNKNOWN0000000000", source));

        let mut state = core.state.lock().unwrap();
        state
            .registrations
            .get_mut("BATTERYCAMERA0001")
            .unwrap()
            .last_seen = Instant::now() - Duration::from_secs(81);
        drop(state);
        assert!(core.registration("BATTERYCAMERA0001").is_none());
    }

    #[test]
    fn client_wake_packet_preserves_client_connection_fields() {
        let client: SocketAddr = "192.168.1.10:40000".parse().unwrap();
        let relay: SocketAddr = "192.168.1.2:58200".parse().unwrap();
        let packet = wake_packet(42, client, relay, 99, -7).unwrap();
        let BcUdpPacket::Discovery(discovery) = BcUdpPacket::decode(&packet).unwrap() else {
            panic!("expected discovery packet");
        };
        let xml = std::str::from_utf8(&discovery.xml).unwrap();
        assert!(xml.contains("<R2D_C>"));
        assert!(xml.contains("<ip>192.168.1.10</ip><port>40000</port>"));
        assert!(xml.contains("<ip>192.168.1.2</ip><port>58200</port>"));
        assert!(xml.contains("<sid>99</sid><cid>-7</cid>"));
    }

    #[test]
    fn parses_client_wake_acknowledgement() {
        let packet = packet(9, "<P2P><R2C_C_R><rsp>0</rsp></R2C_C_R></P2P>");
        assert_eq!(
            decode_message(&packet).unwrap(),
            (9, WakeMessage::ClientConnectReply { response: 0 })
        );
    }

    #[test]
    fn registered_camera_receives_wake_burst_and_client_acknowledgement() {
        let core = core();
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let camera = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        let camera_address = camera.local_addr().unwrap();
        let client_address = client.local_addr().unwrap();
        assert!(core.register("BATTERYCAMERA0001", camera_address));

        let request = client_connect_packet(17, "BATTERYCAMERA0001", client_address, 41).unwrap();
        handle_register(&server, &core, client_address, &request).unwrap();

        camera
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut buffer = [0u8; MAX_DISCOVERY_DATAGRAM];
        let (read, _) = camera.recv_from(&mut buffer).unwrap();
        let BcUdpPacket::Discovery(discovery) = BcUdpPacket::decode(&buffer[..read]).unwrap()
        else {
            panic!("expected wake discovery packet");
        };
        assert!(
            std::str::from_utf8(&discovery.xml)
                .unwrap()
                .contains("<R2D_C>")
        );

        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let (read, _) = client.recv_from(&mut buffer).unwrap();
        assert_eq!(
            decode_message(&buffer[..read]).unwrap().1,
            WakeMessage::ClientConnectReply { response: 0 }
        );
    }

    #[test]
    fn wake_client_uses_registered_server_acknowledgement() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let register_port = server.local_addr().unwrap().port();
        let handle = BatteryWakeHandle {
            core: Arc::new(WakeCore::new(
                BatteryWakeConfig {
                    register_port,
                    ..config()
                },
                HashSet::new(),
            )),
        };
        let server_thread = thread::spawn(move || {
            let mut buffer = [0u8; MAX_DISCOVERY_DATAGRAM];
            let (read, client) = server.recv_from(&mut buffer).unwrap();
            assert!(matches!(
                decode_message(&buffer[..read]).unwrap().1,
                WakeMessage::ClientConnect { .. }
            ));
            let reply = client_connect_reply_packet(23, None, None, 0, 0).unwrap();
            server.send_to(&reply, client).unwrap();
        });
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();

        assert!(
            handle
                .request_wake(
                    &client,
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    "BATTERYCAMERA0001",
                    77,
                    23,
                )
                .unwrap()
        );
        server_thread.join().unwrap();
    }

    #[test]
    fn handle_reports_fresh_and_expired_battery_registration() {
        let started_at = Instant::now();
        let core = Arc::new(WakeCore::new(
            config(),
            HashSet::from(["BATTERYCAMERA0001".to_owned()]),
        ));
        assert!(core.register_at(
            "BATTERYCAMERA0001",
            "192.168.1.20:50000".parse().unwrap(),
            started_at,
        ));
        let handle = BatteryWakeHandle { core };

        assert_eq!(
            handle.health_at("BATTERYCAMERA0001", started_at + Duration::from_secs(20)),
            BatteryWakeHealth {
                registered: true,
                last_seen_age_ms: Some(20_000),
                wake_pending_age_ms: None,
                sleeping: true,
            }
        );
        assert_eq!(
            handle.health_at("BATTERYCAMERA0001", started_at + Duration::from_secs(81)),
            BatteryWakeHealth {
                registered: false,
                last_seen_age_ms: None,
                wake_pending_age_ms: None,
                sleeping: false,
            }
        );
    }

    #[test]
    fn accepted_wake_is_pending_until_media_connects() {
        let core = Arc::new(WakeCore::new(
            config(),
            HashSet::from(["BATTERYCAMERA0001".to_owned()]),
        ));
        assert!(core.register("BATTERYCAMERA0001", "192.168.1.20:50000".parse().unwrap()));
        core.note_wake_requested("BATTERYCAMERA0001");
        let handle = BatteryWakeHandle { core };

        let pending = handle.health("BATTERYCAMERA0001");
        assert!(pending.registered);
        assert!(!pending.sleeping);
        assert!(pending.wake_pending_age_ms.is_some());

        handle.note_media_connected("BATTERYCAMERA0001");
        let recovered = handle.health("BATTERYCAMERA0001");
        assert!(recovered.registered);
        assert!(recovered.sleeping);
        assert_eq!(recovered.wake_pending_age_ms, None);
    }

    #[test]
    fn later_heartbeat_proves_disconnected_camera_returned_to_sleep() {
        let started_at = Instant::now();
        let core = Arc::new(WakeCore::new(
            config(),
            HashSet::from(["BATTERYCAMERA0001".to_owned()]),
        ));
        assert!(core.register_at(
            "BATTERYCAMERA0001",
            "192.168.1.20:50000".parse().unwrap(),
            started_at,
        ));
        core.state
            .lock()
            .unwrap()
            .pending_wakes
            .insert("BATTERYCAMERA0001".to_owned(), started_at);

        assert!(core.register_at(
            "BATTERYCAMERA0001",
            "192.168.1.20:50000".parse().unwrap(),
            started_at + Duration::from_secs(19),
        ));
        assert!(core.wake_pending("BATTERYCAMERA0001").is_some());

        assert!(core.register_at(
            "BATTERYCAMERA0001",
            "192.168.1.20:50000".parse().unwrap(),
            started_at + Duration::from_secs(20),
        ));
        assert_eq!(core.wake_pending("BATTERYCAMERA0001"), None);
    }
}
