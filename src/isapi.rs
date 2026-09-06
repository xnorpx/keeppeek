use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cameras::{Camera, CameraConfig};
use crate::keeppeek::KeepPeekEvent;
use crate::shutdown::Shutdown;
use crate::storage::StorageHandle;
use ::isapi::blocking::Client;
use ::isapi::{Assembler, Bundle, Credentials, PartKind};

pub mod callbacks;
mod lifecycle;
use lifecycle::Tracker;

const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const REPORT_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);
const DELIVERY_TIMEOUT: Duration = Duration::from_millis(500);
const PENDING_TRANSITIONS_MAX: usize = 128;

#[derive(Debug, Clone)]
pub struct Route {
    pub origin: String,
    pub channel: u32,
}

impl Route {
    pub fn for_camera(camera: &Camera) -> Option<Self> {
        if camera.is_reolink || camera.config.username.is_empty() {
            return None;
        }
        let manufacturer = camera
            .config
            .manufacturer_override()
            .or(camera.device.manufacturer.as_deref())
            .or(camera.reported_manufacturer.as_deref())
            .unwrap_or_default();
        let urls = [
            camera.config.main_rtsp_url.as_deref().or_else(|| {
                camera
                    .profiles
                    .first()
                    .and_then(|profile| profile.stream_uri.as_deref())
            }),
            camera.config.sub_rtsp_url.as_deref().or_else(|| {
                camera
                    .profiles
                    .get(1)
                    .and_then(|profile| profile.stream_uri.as_deref())
            }),
        ];
        Self::select(&camera.config, camera.ports.http, manufacturer, urls)
    }

    pub fn for_control(
        config: &CameraConfig,
        http_port: Option<u16>,
        manufacturer: Option<&str>,
    ) -> Option<Self> {
        Self::select(
            config,
            http_port,
            config
                .manufacturer_override()
                .or(manufacturer)
                .unwrap_or_default(),
            [
                config.main_rtsp_url.as_deref(),
                config.sub_rtsp_url.as_deref(),
            ],
        )
    }

    fn select(
        config: &CameraConfig,
        http_port: Option<u16>,
        manufacturer: &str,
        urls: [Option<&str>; 2],
    ) -> Option<Self> {
        if config.backend == crate::cameras::CameraBackend::ReoProto || config.username.is_empty() {
            return None;
        }
        let manufacturer = manufacturer.to_ascii_lowercase();
        let known_brand = ["hikvision", "annke", "hiwatch"]
            .iter()
            .any(|brand| manufacturer.contains(brand));
        let channels = urls
            .into_iter()
            .flatten()
            .filter_map(stream_channel)
            .collect::<Vec<_>>();
        if !known_brand && channels.is_empty() {
            return None;
        }
        let channel = channels.first().copied().unwrap_or(1);
        if channels.iter().any(|candidate| *candidate != channel) {
            return None;
        }
        let port = config.http_port.or(http_port).unwrap_or(80);
        Some(Self {
            origin: format!("http://{}", SocketAddr::new(config.ip, port)),
            channel,
        })
    }
}

fn stream_channel(uri: &str) -> Option<u32> {
    if uri.len() > 4096 {
        return None;
    }
    let url = url::Url::parse(uri).ok()?;
    let path = url.path().trim_matches('/').to_ascii_lowercase();
    let suffix = path
        .strip_prefix("streaming/channels/")
        .or_else(|| path.strip_prefix("isapi/streaming/channels/"))?;
    let stream: u32 = suffix.parse().ok()?;
    ((stream / 100 > 0) && matches!(stream % 100, 1 | 2)).then_some(stream / 100)
}

pub fn spawn(
    camera: &Camera,
    tx: SyncSender<KeepPeekEvent>,
    storage: Option<StorageHandle>,
    shutdown: Shutdown,
) -> anyhow::Result<Option<JoinHandle<()>>> {
    spawn_then(camera, tx, storage, shutdown, || {})
}

pub fn spawn_then(
    camera: &Camera,
    tx: SyncSender<KeepPeekEvent>,
    storage: Option<StorageHandle>,
    shutdown: Shutdown,
    fallback: impl FnOnce() + Send + 'static,
) -> anyhow::Result<Option<JoinHandle<()>>> {
    let Some(route) = Route::for_camera(camera) else {
        return Ok(None);
    };
    let worker = Worker {
        camera: camera.config.clone(),
        tracker: Tracker::new(
            camera.config.ip,
            route.channel,
            camera.config.record_generic_motion_events,
        ),
        route,
        tx,
        storage,
        shutdown,
        counts: Counts::default(),
        pending: VecDeque::new(),
    };
    Ok(Some(
        std::thread::Builder::new()
            .name(format!("isapi-{}", camera.config.ip))
            .spawn(move || {
                if worker.run() {
                    fallback();
                }
            })?,
    ))
}

#[derive(Default)]
struct Counts {
    connections: u64,
    notifications: u64,
    heartbeats: u64,
    ignored: u64,
    images: u64,
    failures: u64,
    transitions: u64,
    delivery_stalls: u64,
    dropped: u64,
}

struct Worker {
    camera: CameraConfig,
    route: Route,
    tracker: Tracker,
    tx: SyncSender<KeepPeekEvent>,
    storage: Option<StorageHandle>,
    shutdown: Shutdown,
    counts: Counts,
    pending: VecDeque<KeepPeekEvent>,
}

#[derive(Debug)]
enum DeliveryError {
    Saturated,
    Closed,
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Saturated => "ISAPI event delivery queue is saturated; transitions retained",
            Self::Closed => "ISAPI event receiver disconnected",
        })
    }
}

impl std::error::Error for DeliveryError {}

impl Worker {
    fn run(mut self) -> bool {
        let mut backoff = RECONNECT_MIN;
        let mut unsupported = false;
        while !self.shutdown.is_cancelled() {
            let before = self.counts.notifications;
            let result = self.observe();
            let changes = self.tracker.disconnect();
            if let Err(error) = self.publish(changes) {
                tracing::warn!(camera_ip = %self.camera.ip, %error, "ISAPI observation cleanup failed");
            }
            if self.shutdown.is_cancelled() {
                break;
            }
            self.counts.failures = self.counts.failures.saturating_add(1);
            let error = result
                .err()
                .unwrap_or_else(|| anyhow::anyhow!("ISAPI event connection closed"));
            let permanent = error
                .downcast_ref::<DeliveryError>()
                .is_some_and(|error| matches!(error, DeliveryError::Closed))
                || error.downcast_ref::<::isapi::Error>().is_some_and(|error| {
                    error.is_authentication()
                        || error.is_invalid_input()
                        || matches!(error.http_status(), Some(401 | 403 | 404 | 405))
                });
            self.report("disconnected");
            tracing::warn!(camera_ip = %self.camera.ip, %error, permanent,
				retry_seconds = backoff.as_secs(), "ISAPI event subscription failed");
            if permanent {
                unsupported = error
                    .downcast_ref::<::isapi::Error>()
                    .is_some_and(|error| matches!(error.http_status(), Some(404 | 405)));
                break;
            }
            if self.counts.notifications > before {
                backoff = RECONNECT_MIN;
            }
            if self.shutdown.wait_timeout(backoff) {
                break;
            }
            backoff = (backoff * 2).min(RECONNECT_MAX);
        }
        if !self.pending.is_empty() {
            self.counts.dropped = self
                .counts
                .dropped
                .saturating_add(self.pending.len() as u64);
            tracing::warn!(camera_ip = %self.camera.ip, pending = self.pending.len(),
                "ISAPI worker stopped before retained transitions could be delivered");
        }
        self.report("stopped");
        unsupported && self.pending.is_empty() && !self.shutdown.is_cancelled()
    }

    fn observe(&mut self) -> anyhow::Result<()> {
        self.publish(Vec::new())?;
        let shutdown = self.shutdown.clone();
        let mut client = Client::builder(
            &self.route.origin,
            Credentials::new(self.camera.username.clone(), self.camera.password.clone()),
        )
        .cancelled(move || shutdown.is_cancelled())
        .build()?;
        let mut stream = client.subscribe()?;
        self.counts.connections = self.counts.connections.saturating_add(1);
        self.report("connected");
        let mut reported = Instant::now();
        let mut notification_deadline = Instant::now() + HEARTBEAT_TIMEOUT;
        let origin = Instant::now();
        let wall_time = unix_time_ms();
        let mut assembler = Assembler::new();
        let result = self.observe_parts(
            &mut stream,
            &mut assembler,
            origin,
            wall_time,
            &mut reported,
            &mut notification_deadline,
        );
        for bundle in assembler.finish() {
            self.deliver_bundle(&bundle, origin, wall_time)?;
        }
        result
    }

    fn observe_parts(
        &mut self,
        stream: &mut ::isapi::blocking::AlertStream,
        assembler: &mut Assembler,
        origin: Instant,
        wall_time: i64,
        reported: &mut Instant,
        notification_deadline: &mut Instant,
    ) -> anyhow::Result<()> {
        while !self.shutdown.is_cancelled() {
            let deadline = assembler
                .next_deadline()
                .map_or(*notification_deadline, |deadline| {
                    (origin + deadline).min(*notification_deadline)
                });
            let Some(part) = stream.next_part_until(deadline)? else {
                return Ok(());
            };
            let received = Instant::now();
            let expired = self.tracker.expire(received);
            self.publish(expired)?;
            if let Some(event) = part.event()? {
                *notification_deadline = received + HEARTBEAT_TIMEOUT;
                self.counts.notifications = self.counts.notifications.saturating_add(1);
                if event.is_heartbeat() {
                    self.counts.heartbeats = self.counts.heartbeats.saturating_add(1);
                }
            } else if part.kind() == PartKind::Jpeg {
                self.counts.images = self.counts.images.saturating_add(1);
            } else {
                self.counts.ignored = self.counts.ignored.saturating_add(1);
            }
            for bundle in assembler.push(part, received.saturating_duration_since(origin))? {
                self.deliver_bundle(&bundle, origin, wall_time)?;
            }
            if received.saturating_duration_since(*reported) >= REPORT_INTERVAL {
                self.report("receiving");
                *reported = received;
            }
        }
        Ok(())
    }

    fn deliver_bundle(
        &mut self,
        bundle: &Bundle,
        origin: Instant,
        wall_time: i64,
    ) -> anyhow::Result<()> {
        let received = origin + bundle.received();
        let time_ms = wall_time.saturating_add(i64::try_from(bundle.received().as_millis())?);
        let outcome = self.tracker.apply_bundle(bundle, received, time_ms)?;
        if outcome.activity {
            if let Some(storage) = &self.storage {
                storage.note_camera_event(&self.camera.ip.to_string());
            }
        } else if outcome.changes.is_empty() && !bundle.event().is_heartbeat() {
            self.counts.ignored = self.counts.ignored.saturating_add(1);
        }
        self.publish(outcome.changes)
    }

    fn publish(&mut self, changes: Vec<KeepPeekEvent>) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.pending
                .iter()
                .chain(&changes)
                .map(lifecycle::queued_bytes)
                .sum::<usize>()
                <= 16 * 1024 * 1024,
            "ISAPI pending image bytes exceed capacity"
        );
        anyhow::ensure!(
            changes.len() <= PENDING_TRANSITIONS_MAX.saturating_sub(self.pending.len()),
            "ISAPI pending transition limit exceeded"
        );
        self.pending.extend(changes);
        let deadline = Instant::now() + DELIVERY_TIMEOUT;
        while let Some(event) = self.pending.pop_front() {
            match self.tx.try_send(event) {
                Ok(()) => {
                    self.counts.transitions = self.counts.transitions.saturating_add(1);
                }
                Err(TrySendError::Full(returned)) => {
                    self.pending.push_front(returned);
                    if Instant::now() >= deadline {
                        self.counts.delivery_stalls = self.counts.delivery_stalls.saturating_add(1);
                        return Err(DeliveryError::Saturated.into());
                    }
                    std::thread::park_timeout(Duration::from_millis(5));
                }
                Err(TrySendError::Disconnected(returned)) => {
                    self.pending.push_front(returned);
                    return Err(DeliveryError::Closed.into());
                }
            }
        }
        Ok(())
    }

    fn report(&self, state: &str) {
        tracing::info!(name: "camera.isapi.status", camera_ip = %self.camera.ip,
			channel = self.route.channel, state,
			connections = self.counts.connections, notifications = self.counts.notifications,
			heartbeats = self.counts.heartbeats, ignored = self.counts.ignored,
			image_parts = self.counts.images, failures = self.counts.failures,
            transitions = self.counts.transitions, delivery_stalls = self.counts.delivery_stalls,
            pending = self.pending.len(), dropped = self.counts.dropped,
			"ISAPI camera event status");
    }
}

fn unix_time_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
