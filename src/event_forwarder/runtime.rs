use super::{
    BrokerFailure,
    broker::{BrokerSession, probe},
    config::MqttForwarderConfig,
    model::{
        EventTransition, Publication, normalize_operational_event, normalize_timeline_event,
        status_topic,
    },
    outbox::{EnqueueResult, Outbox, OutboxStats},
};
use crate::{
    operational_events::OperationalTransition, shutdown::Shutdown, storage::metadata::TimelineEvent,
};
use serde::Serialize;
use std::{
    path::Path,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const INGEST_CAPACITY: usize = 256;
const INGEST_REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_POLL: Duration = Duration::from_millis(100);
const BROKER_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MqttConnectionState {
    Disabled,
    Connecting,
    Connected,
    Degraded,
    OutboxFull,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MqttStatus {
    pub enabled: bool,
    pub state: MqttConnectionState,
    pub detail: String,
    pub connected_at_ms: Option<i64>,
    pub last_received_at_ms: Option<i64>,
    pub last_delivered_at_ms: Option<i64>,
    pub pending_items: u64,
    pub pending_bytes: u64,
    pub oldest_unacknowledged_timestamp_ms: Option<i64>,
    pub retry_count: u64,
    pub duplicate_count: u64,
    pub outbox_limit_bytes: u64,
}

impl MqttStatus {
    fn new(config: &MqttForwarderConfig, stats: OutboxStats) -> Self {
        Self {
            enabled: config.enabled,
            state: if config.enabled {
                MqttConnectionState::Connecting
            } else {
                MqttConnectionState::Disabled
            },
            detail: if config.enabled {
                "Waiting for the MQTT 5 broker.".to_owned()
            } else {
                "MQTT 5 event forwarding is disabled.".to_owned()
            },
            connected_at_ms: None,
            last_received_at_ms: None,
            last_delivered_at_ms: None,
            pending_items: stats.pending_items,
            pending_bytes: stats.pending_bytes,
            oldest_unacknowledged_timestamp_ms: stats.oldest_event_timestamp_ms,
            retry_count: 0,
            duplicate_count: 0,
            outbox_limit_bytes: outbox_limit_bytes(config),
        }
    }
}

enum Command {
    Enqueue {
        publication: Publication,
        reply: SyncSender<Result<EnqueueResult, String>>,
    },
}

#[derive(Clone)]
pub struct Handle {
    sender: SyncSender<Command>,
    config: Arc<RwLock<MqttForwarderConfig>>,
    generation: Arc<AtomicU64>,
    status: Arc<RwLock<MqttStatus>>,
}

impl Handle {
    pub fn publish_timeline(
        &self,
        event: &TimelineEvent,
        transition: EventTransition,
        occurred_at_ms: i64,
    ) -> anyhow::Result<()> {
        let config = self.config();
        if !config.enabled {
            return Ok(());
        }
        let event = normalize_timeline_event(&config, event, transition, occurred_at_ms);
        self.enqueue(Publication::event(&config, &event)?)
            .map(|_| ())
    }

    pub fn publish_operational(&self, transition: &OperationalTransition) -> anyhow::Result<()> {
        let config = self.config();
        if !config.enabled {
            return Ok(());
        }
        let event = normalize_operational_event(&config, transition);
        self.enqueue(Publication::event(&config, &event)?)
            .map(|_| ())
    }

    pub fn update_config(&self, config: MqttForwarderConfig) -> anyhow::Result<()> {
        config.validate()?;
        let mut current = self
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *current = config.clone();
        self.generation.store(config.revision, Ordering::Release);
        let mut status = self
            .status
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        status.enabled = config.enabled;
        status.state = if config.enabled {
            MqttConnectionState::Connecting
        } else {
            MqttConnectionState::Disabled
        };
        status.detail = if config.enabled {
            "Applying MQTT 5 configuration.".to_owned()
        } else {
            "MQTT 5 event forwarding is disabled.".to_owned()
        };
        status.connected_at_ms = None;
        status.outbox_limit_bytes = outbox_limit_bytes(&config);
        Ok(())
    }

    pub fn test_config(&self, config: &MqttForwarderConfig) -> Result<(), BrokerFailure> {
        probe(config, BROKER_OPERATION_TIMEOUT)
    }

    pub fn config(&self) -> MqttForwarderConfig {
        self.config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn status(&self) -> MqttStatus {
        self.status
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn revision(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn enqueue(&self, publication: Publication) -> anyhow::Result<EnqueueResult> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .try_send(Command::Enqueue { publication, reply })
            .map_err(|error| {
                anyhow::anyhow!("MQTT forwarder ingest queue is unavailable: {error}")
            })?;
        result
            .recv_timeout(INGEST_REPLY_TIMEOUT)
            .map_err(|_| anyhow::anyhow!("MQTT forwarder did not persist the event in time"))?
            .map_err(anyhow::Error::msg)
    }
}

pub struct Runtime {
    handle: Handle,
    writer: Option<JoinHandle<()>>,
    publisher: Option<JoinHandle<()>>,
}

impl Runtime {
    pub fn open(
        config: MqttForwarderConfig,
        outbox_path: &Path,
        shutdown: Shutdown,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        let initial_outbox = Outbox::open(outbox_path)?;
        let stats = initial_outbox.stats()?;
        drop(initial_outbox);

        let configuration_revision = config.revision;
        let config = Arc::new(RwLock::new(config));
        let generation = Arc::new(AtomicU64::new(configuration_revision));
        let status = Arc::new(RwLock::new(MqttStatus::new(
            &config
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            stats,
        )));
        let (sender, receiver) = mpsc::sync_channel(INGEST_CAPACITY);
        let handle = Handle {
            sender,
            config: config.clone(),
            generation: generation.clone(),
            status: status.clone(),
        };
        let writer_outbox = outbox_path.to_path_buf();
        let writer_config = config.clone();
        let writer_status = status.clone();
        let writer_shutdown = shutdown.clone();
        let writer = std::thread::Builder::new()
            .name("mqtt-outbox".to_owned())
            .spawn(move || {
                writer_loop(
                    receiver,
                    &writer_outbox,
                    &writer_config,
                    &writer_status,
                    &writer_shutdown,
                );
            })?;

        let publisher_outbox = outbox_path.to_path_buf();
        let publisher_config = config;
        let publisher_status = status;
        let publisher = std::thread::Builder::new()
            .name("mqtt-publisher".to_owned())
            .spawn(move || {
                publisher_loop(
                    &publisher_outbox,
                    &publisher_config,
                    &generation,
                    &publisher_status,
                    &shutdown,
                );
            })?;

        Ok(Self {
            handle,
            writer: Some(writer),
            publisher: Some(publisher),
        })
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub fn join(mut self) {
        if let Some(writer) = self.writer.take()
            && writer.join().is_err()
        {
            tracing::error!("MQTT outbox worker panicked");
        }
        if let Some(publisher) = self.publisher.take()
            && publisher.join().is_err()
        {
            tracing::error!("MQTT publisher worker panicked");
        }
    }
}

fn writer_loop(
    receiver: Receiver<Command>,
    outbox_path: &Path,
    config: &RwLock<MqttForwarderConfig>,
    status: &RwLock<MqttStatus>,
    shutdown: &Shutdown,
) {
    let outbox = match Outbox::open(outbox_path) {
        Ok(outbox) => outbox,
        Err(error) => {
            set_failure(
                status,
                MqttConnectionState::Degraded,
                "MQTT outbox could not be opened.",
            );
            tracing::error!(%error, "unable to open MQTT outbox");
            return;
        }
    };
    while !shutdown.is_cancelled() {
        let command = match receiver.recv_timeout(IDLE_POLL) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match command {
            Command::Enqueue { publication, reply } => {
                let limit = outbox_limit_bytes(
                    &config
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
                let result = outbox
                    .enqueue(&publication, limit, unix_time_ms())
                    .map_err(|error| error.to_string());
                match &result {
                    Ok(EnqueueResult::Inserted) => {
                        if let Ok(stats) = outbox.stats() {
                            let mut health = status
                                .write()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            health.last_received_at_ms = Some(unix_time_ms());
                            apply_stats(&mut health, stats);
                        }
                    }
                    Ok(EnqueueResult::Duplicate) => {
                        status
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .duplicate_count += 1;
                    }
                    Err(error) => {
                        let state = if error.contains("capacity exceeded") {
                            MqttConnectionState::OutboxFull
                        } else {
                            MqttConnectionState::Degraded
                        };
                        set_failure(status, state, error);
                    }
                }
                let _ = reply.send(result);
            }
        }
    }
}

fn publisher_loop(
    outbox_path: &Path,
    config: &RwLock<MqttForwarderConfig>,
    generation: &AtomicU64,
    status: &RwLock<MqttStatus>,
    shutdown: &Shutdown,
) {
    let outbox = match Outbox::open(outbox_path) {
        Ok(outbox) => outbox,
        Err(error) => {
            set_failure(
                status,
                MqttConnectionState::Degraded,
                "MQTT outbox could not be opened.",
            );
            tracing::error!(%error, "unable to open MQTT outbox for delivery");
            return;
        }
    };
    let mut applied_generation = 0;
    let mut session: Option<BrokerSession> = None;
    let mut retry_delay = Duration::ZERO;
    while !shutdown.is_cancelled() {
        let current = config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let current_generation = generation.load(Ordering::Acquire);
        if current_generation != applied_generation {
            if let Some(mut previous) = session.take() {
                let _ = previous.disconnect(Duration::from_secs(1));
            }
            applied_generation = current_generation;
            retry_delay = Duration::ZERO;
        }
        if !current.enabled {
            set_disabled(status, &current, outbox.stats().ok());
            shutdown.wait_timeout(IDLE_POLL);
            continue;
        }
        if !retry_delay.is_zero() && shutdown.wait_timeout(retry_delay) {
            break;
        }
        if session.is_none() {
            match BrokerSession::new(&current).and_then(|mut candidate| {
                candidate.connect(BROKER_OPERATION_TIMEOUT)?;
                Ok(candidate)
            }) {
                Ok(mut connected) => {
                    let online = status_publication(&current, "connected");
                    if let Err(error) = connected.publish(&online, BROKER_OPERATION_TIMEOUT) {
                        note_retry(status, &error.detail, outbox.stats().ok());
                        retry_delay = next_retry_delay(retry_delay, &current);
                        continue;
                    }
                    set_connected(status, outbox.stats().ok());
                    retry_delay = Duration::ZERO;
                    session = Some(connected);
                }
                Err(error) => {
                    note_retry(status, &error.detail, outbox.stats().ok());
                    retry_delay = next_retry_delay(retry_delay, &current);
                    continue;
                }
            }
        }

        let item = match outbox.next() {
            Ok(item) => item,
            Err(error) => {
                tracing::warn!(%error, "unable to read MQTT outbox");
                set_failure(
                    status,
                    MqttConnectionState::Degraded,
                    "MQTT outbox could not be read.",
                );
                shutdown.wait_timeout(IDLE_POLL);
                continue;
            }
        };
        let Some(item) = item else {
            if let Some(connected) = session.as_mut()
                && let Err(error) = connected.poll(IDLE_POLL)
            {
                note_retry(status, &error.detail, outbox.stats().ok());
                session = None;
                retry_delay = next_retry_delay(retry_delay, &current);
            }
            continue;
        };

        let delivery = session
            .as_mut()
            .expect("MQTT session must exist before delivery")
            .publish(&item.publication, BROKER_OPERATION_TIMEOUT);
        match delivery {
            Ok(()) => {
                let delivered_at_ms = unix_time_ms();
                if let Err(error) = outbox.mark_delivered(item.sequence, delivered_at_ms) {
                    tracing::warn!(%error, "unable to acknowledge MQTT outbox delivery");
                    set_failure(
                        status,
                        MqttConnectionState::Degraded,
                        "MQTT delivery was acknowledged but its outbox receipt could not be saved.",
                    );
                } else {
                    let mut health = status
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    health.state = MqttConnectionState::Connected;
                    health.detail = "MQTT 5 broker is connected.".to_owned();
                    health.last_delivered_at_ms = Some(delivered_at_ms);
                    if let Ok(stats) = outbox.stats() {
                        apply_stats(&mut health, stats);
                    }
                }
                retry_delay = Duration::ZERO;
            }
            Err(error) => {
                if let Err(mark_error) = outbox.mark_attempt(item.sequence, &error.detail) {
                    tracing::warn!(%mark_error, "unable to record MQTT delivery attempt");
                }
                note_retry(status, &error.detail, outbox.stats().ok());
                session = None;
                retry_delay = next_retry_delay(retry_delay, &current);
            }
        }
    }
    if let Some(mut connected) = session {
        let offline = status_publication(
            &config
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            "disconnected",
        );
        let _ = connected.publish(&offline, Duration::from_secs(1));
        let _ = connected.disconnect(Duration::from_secs(1));
    }
}

fn status_publication(config: &MqttForwarderConfig, state: &str) -> Publication {
    Publication {
        dedup_key: format!("status:{}:{state}", config.forwarder_id),
        topic: status_topic(config),
        payload: serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "forwarder_id": config.forwarder_id,
            "state": state,
            "timestamp_ms": unix_time_ms(),
        }))
        .unwrap_or_default(),
        qos: config.qos,
        retain: config.retain_health,
        event_timestamp_ms: unix_time_ms(),
        content_type: "application/json".to_owned(),
        payload_format_indicator: Some(1),
        correlation_data: config.forwarder_id.as_bytes().to_vec(),
    }
}

fn set_disabled(
    status: &RwLock<MqttStatus>,
    config: &MqttForwarderConfig,
    stats: Option<OutboxStats>,
) {
    let mut health = status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    health.enabled = false;
    health.state = MqttConnectionState::Disabled;
    health.detail = "MQTT 5 event forwarding is disabled.".to_owned();
    health.connected_at_ms = None;
    health.outbox_limit_bytes = outbox_limit_bytes(config);
    if let Some(stats) = stats {
        apply_stats(&mut health, stats);
    }
}

fn set_connected(status: &RwLock<MqttStatus>, stats: Option<OutboxStats>) {
    let mut health = status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    health.enabled = true;
    health.state = MqttConnectionState::Connected;
    health.detail = "MQTT 5 broker is connected.".to_owned();
    health.connected_at_ms = Some(unix_time_ms());
    if let Some(stats) = stats {
        apply_stats(&mut health, stats);
    }
}

fn note_retry(status: &RwLock<MqttStatus>, detail: &str, stats: Option<OutboxStats>) {
    let mut health = status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    health.enabled = true;
    health.state = MqttConnectionState::Degraded;
    health.detail = detail.to_owned();
    health.connected_at_ms = None;
    health.retry_count = health.retry_count.saturating_add(1);
    if let Some(stats) = stats {
        apply_stats(&mut health, stats);
    }
}

fn set_failure(status: &RwLock<MqttStatus>, state: MqttConnectionState, detail: &str) {
    let mut health = status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    health.state = state;
    health.detail = detail.to_owned();
    health.connected_at_ms = None;
}

const fn apply_stats(status: &mut MqttStatus, stats: OutboxStats) {
    status.pending_items = stats.pending_items;
    status.pending_bytes = stats.pending_bytes;
    status.oldest_unacknowledged_timestamp_ms = stats.oldest_event_timestamp_ms;
}

fn next_retry_delay(current: Duration, config: &MqttForwarderConfig) -> Duration {
    let minimum = Duration::from_millis(config.retry_min_ms);
    let maximum = Duration::from_millis(config.retry_max_ms);
    if current.is_zero() {
        minimum
    } else {
        current.saturating_mul(2).min(maximum)
    }
}

const fn outbox_limit_bytes(config: &MqttForwarderConfig) -> u64 {
    config.outbox_max_mb.saturating_mul(1_024 * 1_024)
}

fn unix_time_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::metadata::EventSource;
    use std::{
        io::{self, Read, Write},
        net::{TcpListener, TcpStream},
        time::Instant,
    };

    #[test]
    fn retry_delay_is_bounded() {
        let config = MqttForwarderConfig {
            retry_min_ms: 10,
            retry_max_ms: 40,
            ..MqttForwarderConfig::default()
        };
        assert_eq!(
            next_retry_delay(Duration::ZERO, &config),
            Duration::from_millis(10)
        );
        assert_eq!(
            next_retry_delay(Duration::from_millis(10), &config),
            Duration::from_millis(20)
        );
        assert_eq!(
            next_retry_delay(Duration::from_millis(40), &config),
            Duration::from_millis(40)
        );
    }

    #[test]
    fn status_payload_never_contains_credentials() {
        let config = MqttForwarderConfig {
            username: Some("operator".to_owned()),
            password: Some("super-secret".to_owned()),
            ..MqttForwarderConfig::default()
        };
        let publication = status_publication(&config, "connected");
        let payload = String::from_utf8(publication.payload).unwrap();
        assert!(!payload.contains("operator"));
        assert!(!payload.contains("super-secret"));
    }

    #[test]
    fn broker_outage_replays_normalized_motion_event_after_restart() {
        let unavailable = TcpListener::bind("127.0.0.1:0").unwrap();
        let unavailable_address = unavailable.local_addr().unwrap();
        drop(unavailable);
        let recovery_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let recovery_address = recovery_listener.local_addr().unwrap();
        let mut config = MqttForwarderConfig {
            enabled: true,
            broker_url: format!("mqtt://{unavailable_address}"),
            outbox_max_mb: 1,
            retry_min_ms: 10,
            retry_max_ms: 40,
            ..MqttForwarderConfig::default()
        };
        let outbox_path = std::env::temp_dir().join(format!(
            "keeppeek-mqtt-recovery-{}.db",
            uuid::Uuid::new_v4()
        ));
        let shutdown = Shutdown::new();
        let runtime = Runtime::open(config.clone(), &outbox_path, shutdown.clone()).unwrap();
        let handle = runtime.handle();
        handle
            .publish_timeline(&motion_event(), EventTransition::Created, 1_786_800_000_000)
            .unwrap();
        wait_until(Duration::from_secs(10), || {
            let status = handle.status();
            status.pending_items == 1 && status.retry_count > 0
        });
        shutdown.cancel();
        runtime.join();

        let (published, received) = mpsc::channel();
        let broker = std::thread::spawn(move || serve_broker(recovery_listener, &published));
        config.broker_url = format!("mqtt://{recovery_address}");
        let recovery_shutdown = Shutdown::new();
        let recovered = Runtime::open(config, &outbox_path, recovery_shutdown.clone()).unwrap();
        let recovered_handle = recovered.handle();
        let (topic, payload) = received.recv_timeout(BROKER_OPERATION_TIMEOUT).unwrap();
        assert_eq!(topic, "keeppeek/home-nvr/sources/front-door/events/motion");
        let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(payload["schema_version"], 1);
        assert_eq!(payload["event_id"], "motion-42");
        assert_eq!(payload["revision"], 1);
        assert_eq!(payload["transition"], "created");
        wait_until(Duration::from_secs(10), || {
            recovered_handle.status().pending_items == 0
        });
        assert_eq!(
            recovered_handle.status().state,
            MqttConnectionState::Connected
        );

        recovery_shutdown.cancel();
        recovered.join();
        broker.join().unwrap();
        let _ = std::fs::remove_file(outbox_path);
    }

    #[test]
    #[ignore = "performance evidence"]
    fn mqtt_enqueue_latency_benchmark() {
        const RUNS: u64 = 128;
        const P95_BUDGET_NS: u64 = 50_000_000;

        let outbox_path =
            std::env::temp_dir().join(format!("keeppeek-mqtt-latency-{}.db", uuid::Uuid::new_v4()));
        let shutdown = Shutdown::new();
        let runtime = Runtime::open(
            MqttForwarderConfig::default(),
            &outbox_path,
            shutdown.clone(),
        )
        .unwrap();
        let handle = runtime.handle();
        let mut baseline = hdrhistogram::Histogram::<u64>::new(3).unwrap();
        for index in 0..RUNS {
            let mut event = motion_event();
            event.id = format!("disabled-{index}");
            let started = Instant::now();
            handle
                .publish_timeline(&event, EventTransition::Created, event.start_time_ms)
                .unwrap();
            baseline
                .record(u64::try_from(started.elapsed().as_nanos()).unwrap())
                .unwrap();
        }

        handle
            .update_config(MqttForwarderConfig {
                revision: 2,
                enabled: true,
                broker_url: "mqtt://127.0.0.1:9".to_owned(),
                ..MqttForwarderConfig::default()
            })
            .unwrap();
        let mut enabled = hdrhistogram::Histogram::<u64>::new(3).unwrap();
        for index in 0..RUNS {
            let mut event = motion_event();
            event.id = format!("enabled-{index}");
            let started = Instant::now();
            handle
                .publish_timeline(&event, EventTransition::Created, event.start_time_ms)
                .unwrap();
            enabled
                .record(u64::try_from(started.elapsed().as_nanos()).unwrap())
                .unwrap();
        }
        let baseline_p50 = baseline.value_at_quantile(0.5);
        let baseline_p95 = baseline.value_at_quantile(0.95);
        let enabled_p50 = enabled.value_at_quantile(0.5);
        let enabled_p95 = enabled.value_at_quantile(0.95);
        println!(
            "mqtt_enqueue_latency_ns runs={RUNS} baseline_p50={baseline_p50} baseline_p95={baseline_p95} enabled_p50={enabled_p50} enabled_p95={enabled_p95} delta_p95={} budget_p95={P95_BUDGET_NS}",
            enabled_p95.saturating_sub(baseline_p95)
        );
        assert!(
            enabled_p95 <= P95_BUDGET_NS,
            "enabled MQTT enqueue P95 {enabled_p95} ns exceeds {P95_BUDGET_NS} ns"
        );

        shutdown.cancel();
        runtime.join();
        let _ = std::fs::remove_file(outbox_path);
    }

    fn motion_event() -> TimelineEvent {
        TimelineEvent {
            id: "motion-42".to_owned(),
            revision: 1,
            camera_id: "front-door".to_owned(),
            stream: Some("sub".to_owned()),
            source: EventSource::Camera,
            kind: "motion".to_owned(),
            start_time_ms: 1_786_800_000_000,
            end_time_ms: None,
            confidence: Some(0.91),
            bbox: None,
            bbox_attachment_id: None,
            zone: Some("porch".to_owned()),
            attachments: Vec::new(),
            canonical_attachment_id: None,
            icon_key: "motion".to_owned(),
            rejected_icon_key: None,
            thumbnail_filename: None,
        }
    }

    fn serve_broker(listener: TcpListener, published: &mpsc::Sender<(String, Vec<u8>)>) {
        let (mut stream, _) = listener.accept().unwrap();
        let (_, connect) = read_frame(&mut stream).unwrap().unwrap();
        assert_eq!(connect[6], 5);
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        while let Some((header, body)) = read_frame(&mut stream).unwrap() {
            match header >> 4 {
                3 => {
                    let (topic, payload, packet_id) = published_message(header, &body);
                    if topic.contains("/events/") {
                        published.send((topic, payload)).unwrap();
                    }
                    if let Some(packet_id) = packet_id {
                        let packet_id = packet_id.to_be_bytes();
                        stream
                            .write_all(&[0x40, 0x02, packet_id[0], packet_id[1]])
                            .unwrap();
                    }
                }
                14 => break,
                _ => {}
            }
        }
    }

    fn published_message(header: u8, body: &[u8]) -> (String, Vec<u8>, Option<u16>) {
        let topic_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
        let topic_end = 2 + topic_length;
        let topic = std::str::from_utf8(&body[2..topic_end]).unwrap().to_owned();
        let qos = (header >> 1) & 0x03;
        let mut cursor = topic_end;
        let packet_id = if qos == 0 {
            None
        } else {
            let id = u16::from_be_bytes([body[cursor], body[cursor + 1]]);
            cursor += 2;
            Some(id)
        };
        let (properties_length, properties_prefix) = variable_integer(&body[cursor..]);
        cursor += properties_prefix + properties_length;
        (topic, body[cursor..].to_vec(), packet_id)
    }

    fn read_frame(stream: &mut TcpStream) -> io::Result<Option<(u8, Vec<u8>)>> {
        let mut header = [0_u8; 1];
        match stream.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let mut multiplier = 1_usize;
        let mut remaining = 0_usize;
        loop {
            let mut encoded = [0_u8; 1];
            stream.read_exact(&mut encoded)?;
            remaining += usize::from(encoded[0] & 0x7f) * multiplier;
            if encoded[0] & 0x80 == 0 {
                break;
            }
            multiplier *= 128;
        }
        let mut body = vec![0_u8; remaining];
        stream.read_exact(&mut body)?;
        Ok(Some((header[0], body)))
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

    fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + timeout;
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "condition did not become true in time"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
