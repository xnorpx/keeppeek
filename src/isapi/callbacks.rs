use std::collections::BTreeMap;
use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::SyncSender,
};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::lifecycle::Tracker;
use crate::keeppeek::KeepPeekEvent;
use crate::shutdown::Shutdown;

mod pending;
use pending::{Pending, State};

const BODY_BYTES_MAX: usize = 8 * 1024 * 1024;
const INGRESS_BYTES_MAX: usize = 32 * 1024 * 1024;
const PATH_PREFIX: &str = "/ISAPI/Event/notification/callback/";
const COMMIT_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub bind: SocketAddr,
    #[serde(default)]
    pub trusted_proxy: Option<IpAddr>,
    pub sources: Vec<Source>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub ip: IpAddr,
    pub channel: u32,
    pub username: String,
    pub password: String,
}

impl Config {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.sources.is_empty() && self.sources.len() <= 64,
            "ISAPI callbacks require 1 to 64 configured sources"
        );
        let mut ips = std::collections::BTreeSet::new();
        let mut users = std::collections::BTreeSet::new();
        for source in &self.sources {
            anyhow::ensure!(
                source.channel > 0
                    && !source.ip.is_unspecified()
                    && !source.ip.is_multicast()
                    && ips.insert(source.ip)
                    && users.insert(&source.username),
                "ISAPI callback source identity is invalid or duplicated"
            );
            ::isapi::CallbackAuth::new(::isapi::Credentials::new(
                source.username.clone(),
                source.password.clone(),
            ))?;
        }
        anyhow::ensure!(
            self.trusted_proxy
                .is_none_or(|ip| !ip.is_unspecified() && !ip.is_multicast()),
            "invalid ISAPI callback proxy address"
        );
        Ok(())
    }
}

struct Entry {
    epoch: Arc<std::sync::atomic::AtomicU64>,
    generic_motion: AtomicBool,
    channel: u32,
    active: AtomicBool,
    auth: Mutex<::isapi::CallbackAuth>,
    state: Mutex<State>,
}

struct Receiver {
    entries: BTreeMap<IpAddr, Entry>,
    trusted_proxy: Option<IpAddr>,
    origin: Instant,
    tx: SyncSender<KeepPeekEvent>,
    shutdown: Shutdown,
    bytes: Arc<AtomicUsize>,
}

impl Receiver {
    fn new(
        config: &Config,
        tx: SyncSender<KeepPeekEvent>,
        shutdown: Shutdown,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        let entries = config
            .sources
            .iter()
            .map(|source| {
                Ok((
                    source.ip,
                    Entry {
                        epoch: Arc::new(std::sync::atomic::AtomicU64::new(1)),
                        generic_motion: AtomicBool::new(false),
                        channel: source.channel,
                        active: AtomicBool::new(true),
                        auth: Mutex::new(::isapi::CallbackAuth::new(::isapi::Credentials::new(
                            source.username.clone(),
                            source.password.clone(),
                        ))?),
                        state: Mutex::new(State::new(Tracker::new(
                            source.ip,
                            source.channel,
                            false,
                        ))),
                    },
                ))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(Self {
            entries,
            trusted_proxy: config.trusted_proxy,
            origin: Instant::now(),
            tx,
            shutdown,
            bytes: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn handle(&self, request: &rouille::Request) -> rouille::Response {
        let result = self.handle_inner(request);
        let response = match result {
            Ok(response) => response,
            Err(status) => response(status),
        };
        tracing::debug!(name: "camera.isapi.callback", peer = %request.remote_addr().ip(), http_status = response.status_code, pending_bytes = self.bytes.load(Ordering::Relaxed), "ISAPI callback handled");
        response
    }

    fn handle_inner(&self, request: &rouille::Request) -> Result<rouille::Response, u16> {
        if self.shutdown.is_cancelled() {
            return Err(503);
        }
        if request.method() != "POST" {
            return Err(405);
        }
        if request.header("Origin").is_some() {
            return Err(403);
        }
        let ip = request
            .raw_url()
            .strip_prefix(PATH_PREFIX)
            .and_then(|ip| ip.parse::<IpAddr>().ok())
            .ok_or(403_u16)?;
        let entry = self
            .entries
            .get(&ip)
            .filter(|entry| entry.active.load(Ordering::Acquire))
            .ok_or(403_u16)?;
        let generation = entry.epoch.load(Ordering::Acquire);
        let peer = request.remote_addr().ip();
        if peer != ip && self.trusted_proxy != Some(peer) {
            return Err(403);
        }
        let authorization = header(request, "authorization")?;
        let mut auth = entry.auth.try_lock().map_err(|_| 429_u16)?;
        if authorization.is_none_or(|value| {
            auth.verify(
                value,
                request.method(),
                request.raw_url(),
                self.origin.elapsed(),
            )
            .is_err()
        }) {
            let challenge = auth
                .challenge(
                    &format!("{:032x}", rand::random::<u128>()),
                    self.origin.elapsed(),
                )
                .map_err(|_| 503_u16)?;
            return Ok(response(401).with_additional_header("WWW-Authenticate", challenge));
        }
        drop(auth);
        if header(request, "content-encoding")?.is_some_and(|value| value != "identity") {
            return Err(415);
        }
        if let Some(length) = header(request, "content-length")?
            && length.parse::<usize>().map_err(|_| 400_u16)? > BODY_BYTES_MAX
        {
            return Err(413);
        }
        let content_type = header(request, "content-type")?.ok_or(415_u16)?;
        let permit = Permit::acquire(Arc::clone(&self.bytes))?;
        let body = read_body(request, &self.shutdown)?;
        if !entry.active.load(Ordering::Acquire)
            || entry.epoch.load(Ordering::Acquire) != generation
        {
            return Err(403);
        }
        let hash: [u8; 32] = Sha256::digest(&body).into();
        let mut state = entry.state.try_lock().map_err(|_| 429_u16)?;
        if !entry.active.load(Ordering::Acquire)
            || entry.epoch.load(Ordering::Acquire) != generation
        {
            return Err(403);
        }
        state.reconcile();
        if !state.synchronize(
            generation,
            ip,
            entry.channel,
            entry.generic_motion.load(Ordering::Acquire),
        ) {
            return Err(503);
        }
        if state.seen(hash, Instant::now()) {
            return Ok(response(200));
        }
        if let Some(pending) = &state.pending {
            if pending.hash != Some(hash) {
                return Err(503);
            }
        } else {
            let bundles = decode(content_type, body).map_err(|_| 400_u16)?;
            if bundles.iter().any(|bundle| {
                !bundle.event().is_heartbeat()
                    && bundle
                        .event()
                        .dynamic_channel_id()
                        .or_else(|| bundle.event().channel_id())
                        != Some(entry.channel)
            }) {
                return Err(400);
            }
            let mut tracker = state.tracker.clone();
            let mut changes = Vec::new();
            for bundle in bundles {
                for image in bundle.images() {
                    crate::storage::events::jpeg_dimensions(image.body()).map_err(|_| 400_u16)?;
                }
                let outcome = tracker
                    .apply_bundle(&bundle, Instant::now(), super::unix_time_ms())
                    .map_err(|_| 400_u16)?;
                changes.extend(outcome.changes);
                if changes.len() > 128
                    || changes
                        .iter()
                        .map(super::lifecycle::queued_bytes)
                        .sum::<usize>()
                        > 16 * 1024 * 1024
                {
                    return Err(413);
                }
            }
            state.pending = Some(
                Pending::new(Some(hash), tracker, changes, Some(permit)).fenced(Fence {
                    epoch: Arc::clone(&entry.epoch),
                    generation,
                }),
            );
        }
        state.submit(&self.tx).map_err(|_| 503_u16)?;
        state.wait(COMMIT_WAIT);
        if state.seen(hash, Instant::now()) {
            Ok(response(200))
        } else {
            Err(503)
        }
    }

    fn maintain(&self) {
        for (ip, entry) in &self.entries {
            let Ok(mut state) = entry.state.try_lock() else {
                continue;
            };
            state.reconcile();
            if !state.synchronize(
                entry.epoch.load(Ordering::Acquire),
                *ip,
                entry.channel,
                entry.generic_motion.load(Ordering::Acquire),
            ) {
                continue;
            }
            if state.pending.is_none() {
                let mut tracker = state.tracker.clone();
                let changes = if entry.active.load(Ordering::Acquire) {
                    tracker.expire(Instant::now())
                } else {
                    tracker.disconnect()
                };
                if !changes.is_empty() {
                    state.pending = Some(Pending::new(None, tracker, changes, None));
                }
            }
            if let Err(error) = state.submit(&self.tx) {
                tracing::debug!(%error, "ISAPI callback commit remains pending");
            }
        }
    }
}

struct Permit {
    bytes: Arc<AtomicUsize>,
}
impl Permit {
    fn acquire(bytes: Arc<AtomicUsize>) -> Result<Self, u16> {
        bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current <= INGRESS_BYTES_MAX - BODY_BYTES_MAX).then_some(current + BODY_BYTES_MAX)
            })
            .map_err(|_| 503_u16)?;
        Ok(Self { bytes })
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.bytes.fetch_sub(BODY_BYTES_MAX, Ordering::AcqRel);
    }
}

fn header<'request>(
    request: &'request rouille::Request,
    name: &str,
) -> Result<Option<&'request str>, u16> {
    let mut headers = request
        .headers()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let value = headers.next().map(|(_, value)| value);
    if headers.next().is_some() {
        return Err(400);
    }
    Ok(value)
}

fn read_body(request: &rouille::Request, shutdown: &Shutdown) -> Result<Vec<u8>, u16> {
    let mut input = request.data().ok_or(400_u16)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut body = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        if shutdown.is_cancelled() {
            return Err(503);
        }
        if Instant::now() >= deadline {
            return Err(408);
        }
        let count = input.read(&mut buffer).map_err(|_| 408_u16)?;
        if count == 0 {
            break;
        }
        if count > BODY_BYTES_MAX.saturating_sub(body.len()) {
            return Err(413);
        }
        body.extend_from_slice(&buffer[..count]);
    }
    Ok(body)
}

fn decode(content_type: &str, body: Vec<u8>) -> anyhow::Result<Vec<::isapi::Bundle>> {
    anyhow::ensure!(
        content_type.len() <= 8192,
        "ISAPI callback media type exceeds limit"
    );
    let media: mime::Mime = content_type.parse()?;
    let mut assembler = ::isapi::Assembler::new();
    let mut bundles = Vec::new();
    let mut count = 0;
    if media.type_() == mime::MULTIPART {
        let mut decoder = ::isapi::Decoder::new(content_type)?;
        for chunk in body.chunks(8192) {
            decoder.push(chunk)?;
            while let Some(part) = decoder.next_part()? {
                count += 1;
                anyhow::ensure!(count <= 128, "ISAPI callback part count exceeded");
                bundles.extend(assembler.push(part, Duration::ZERO)?);
                anyhow::ensure!(bundles.len() <= 64, "ISAPI callback event count exceeded");
            }
        }
        decoder.finish()?;
    } else {
        anyhow::ensure!(
            matches!(
                media.essence_str(),
                "application/xml" | "text/xml" | "application/json"
            ),
            "unsupported ISAPI callback media type"
        );
        bundles.extend(assembler.push(
            ::isapi::Part::from_body(content_type, body)?,
            Duration::ZERO,
        )?);
    }
    bundles.extend(assembler.finish());
    anyhow::ensure!(
        !bundles.is_empty() && bundles.len() <= 64,
        "ISAPI callback has no events or exceeds its limit"
    );
    Ok(bundles)
}

fn response(status: u16) -> rouille::Response {
    rouille::Response::text(match status {
        200 => "OK",
        401 => "Authentication required",
        403 => "Sender not allowed",
        400 => "Invalid callback",
        413 => "Callback exceeds limit",
        415 => "Unsupported media type",
        405 => "POST required",
        408 => "Callback deadline expired",
        429 => "Sender busy",
        _ => "Temporarily unavailable",
    })
    .with_status_code(status)
    .with_additional_header("Cache-Control", "no-store")
    .with_additional_header("X-Content-Type-Options", "nosniff")
    .with_additional_header("Connection", "close")
}

pub struct Runtime {
    #[cfg(test)]
    address: SocketAddr,
    receiver: Arc<Receiver>,
    handle: Option<std::thread::JoinHandle<()>>,
    stop: Shutdown,
}

impl Runtime {
    #[cfg(test)]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn start(
        config: &Config,
        tx: SyncSender<KeepPeekEvent>,
        parent: Shutdown,
    ) -> anyhow::Result<Self> {
        let stop = Shutdown::new();
        let receiver = Arc::new(Receiver::new(config, tx, stop.clone())?);
        let handler = Arc::clone(&receiver);
        let listener = std::net::TcpListener::bind(config.bind)?;
        let server = rouille::Server::from_tcp_listener_single_request(listener, move |request| {
            handler.handle(request)
        })
        .map_err(|_| anyhow::anyhow!("unable to bind ISAPI callback listener"))?
        .pool_size(4);
        let address = server.server_addr();
        let worker = Arc::clone(&receiver);
        let shutdown = stop.clone();
        let handle = std::thread::Builder::new()
            .name("isapi-callbacks".to_owned())
            .spawn(move || {
                tracing::info!(%address, "ISAPI callback listener started");
                while !shutdown.is_cancelled() && !parent.is_cancelled() {
                    server.poll_once_timeout(Duration::from_millis(100));
                    worker.maintain();
                }
                shutdown.cancel();
                if !server.join_timeout(Duration::from_secs(35)) {
                    tracing::warn!("ISAPI callback requests exceeded shutdown grace period");
                }
                worker.maintain();
                tracing::info!("ISAPI callback listener stopped");
            })?;
        Ok(Self {
            #[cfg(test)]
            address,
            receiver,
            handle: Some(handle),
            stop,
        })
    }
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.receiver.entries.contains_key(&ip)
    }
    pub fn activate(&self, ip: IpAddr, generic_motion: bool) {
        if let Some(entry) = self.receiver.entries.get(&ip) {
            entry
                .generic_motion
                .store(generic_motion, Ordering::Release);
            bump_epoch(&entry.epoch);
            entry.active.store(true, Ordering::Release);
        }
    }
    pub fn deactivate(&self, ip: IpAddr) {
        if let Some(entry) = self.receiver.entries.get(&ip) {
            entry.active.store(false, Ordering::Release);
            bump_epoch(&entry.epoch);
        }
    }
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }
    pub fn cancel(&self) {
        self.stop.cancel();
        for entry in self.receiver.entries.values() {
            entry.active.store(false, Ordering::Release);
            bump_epoch(&entry.epoch);
        }
    }
    pub fn join(mut self) {
        self.cancel();
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            tracing::error!("ISAPI callback listener panicked");
        }
    }
}

#[derive(Clone)]
pub struct Fence {
    epoch: Arc<std::sync::atomic::AtomicU64>,
    generation: u64,
}

impl Fence {
    pub fn is_current(&self) -> bool {
        self.epoch.load(Ordering::Acquire) == self.generation
    }
}

fn bump_epoch(epoch: &std::sync::atomic::AtomicU64) {
    epoch
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        })
        .expect("callback generation space exhausted");
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.cancel();
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            tracing::error!("ISAPI callback listener panicked during cleanup");
        }
    }
}

#[cfg(test)]
mod tests;
