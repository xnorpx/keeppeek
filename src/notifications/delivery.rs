use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Stage, model::Channel, store::Store};

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(5);
const DELIVERY_IDLE_INTERVAL: Duration = Duration::from_millis(100);
const MIN_RETRY_INTERVAL_MS: u64 = 1_000;
const MAX_REASON_BYTES: usize = 256;

#[derive(Debug, Clone)]
pub(super) struct Delivery {
    id: i64,
    logical_id: String,
    rule_id: String,
    stage: Stage,
    channel: Channel,
    destination_json: String,
    payload_json: String,
    replacement_key: String,
    attempt: u32,
    max_attempts: u32,
    max_retry_interval_ms: u64,
    attachment_path: Option<String>,
    attachment_enabled: bool,
    attachment_required: bool,
    max_attachment_bytes: u64,
    expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeliveryOutcome {
    Delivered {
        provider_status: Option<u16>,
    },
    Transient {
        reason: String,
        provider_status: Option<u16>,
        retry_after_ms: Option<u64>,
    },
    Permanent {
        reason: String,
        provider_status: Option<u16>,
    },
}

trait Provider: Send + Sync {
    fn deliver(&self, delivery: &Delivery) -> DeliveryOutcome;
}

struct BrowserProvider;

impl Provider for BrowserProvider {
    fn deliver(&self, _delivery: &Delivery) -> DeliveryOutcome {
        DeliveryOutcome::Delivered {
            provider_status: None,
        }
    }
}

struct UnavailableProvider;

impl Provider for UnavailableProvider {
    fn deliver(&self, _delivery: &Delivery) -> DeliveryOutcome {
        DeliveryOutcome::Permanent {
            reason: "channel_unavailable".to_owned(),
            provider_status: None,
        }
    }
}

struct WebhookProvider {
    agent: ureq::Agent,
}

#[derive(Deserialize)]
struct Destination {
    value: String,
}

#[derive(Deserialize, Serialize)]
struct WebhookPayload {
    title: String,
    body: String,
    deep_link: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attachment_content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attachment_base64: Option<String>,
}

impl WebhookProvider {
    fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(WEBHOOK_TIMEOUT))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Provider for WebhookProvider {
    fn deliver(&self, delivery: &Delivery) -> DeliveryOutcome {
        let destination: Destination = match serde_json::from_str(&delivery.destination_json) {
            Ok(destination) => destination,
            Err(_) => {
                return DeliveryOutcome::Permanent {
                    reason: "destination_invalid".to_owned(),
                    provider_status: None,
                };
            }
        };
        let payload = match webhook_payload(delivery) {
            Ok(payload) => payload,
            Err(reason) => {
                return DeliveryOutcome::Permanent {
                    reason: reason.to_owned(),
                    provider_status: None,
                };
            }
        };
        let response = self
            .agent
            .post(&destination.value)
            .header("Content-Type", "application/json")
            .header("X-KeepPeek-Collapse-Key", &delivery.replacement_key)
            .header("X-KeepPeek-Stage", stage_str(delivery.stage))
            .send(payload.as_slice());
        match response {
            Ok(response) => {
                let status = response.status().as_u16();
                if response.status().is_success() {
                    DeliveryOutcome::Delivered {
                        provider_status: Some(status),
                    }
                } else if status == 408 || status == 425 || status == 429 || status >= 500 {
                    DeliveryOutcome::Transient {
                        reason: "provider_retryable_status".to_owned(),
                        provider_status: Some(status),
                        retry_after_ms: retry_after_ms(response.headers()),
                    }
                } else {
                    DeliveryOutcome::Permanent {
                        reason: "provider_permanent_status".to_owned(),
                        provider_status: Some(status),
                    }
                }
            }
            Err(ureq::Error::Timeout(_)) => DeliveryOutcome::Transient {
                reason: "provider_timeout".to_owned(),
                provider_status: None,
                retry_after_ms: None,
            },
            Err(ureq::Error::HostNotFound | ureq::Error::ConnectionFailed) => {
                DeliveryOutcome::Transient {
                    reason: "provider_unreachable".to_owned(),
                    provider_status: None,
                    retry_after_ms: None,
                }
            }
            Err(_) => DeliveryOutcome::Transient {
                reason: "provider_transport_error".to_owned(),
                provider_status: None,
                retry_after_ms: None,
            },
        }
    }
}

pub(super) struct Workers {
    shutdown: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Workers {
    pub(super) fn start(path: &Path) -> anyhow::Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::with_capacity(4);
        let channels = [
            Channel::Browser,
            Channel::Push,
            Channel::Webhook,
            Channel::Forwarder,
        ];
        let stores = channels
            .iter()
            .map(|_| Store::open(path))
            .collect::<anyhow::Result<Vec<_>>>()?;
        for (channel, store) in channels.into_iter().zip(stores) {
            let worker_shutdown = shutdown.clone();
            let provider: Box<dyn Provider> = match channel {
                Channel::Browser => Box::new(BrowserProvider),
                Channel::Webhook => Box::new(WebhookProvider::new()),
                Channel::Push | Channel::Forwarder => Box::new(UnavailableProvider),
            };
            threads.push(
                std::thread::Builder::new()
                    .name(format!("notification-{}", channel.as_str()))
                    .spawn(move || run_worker(store, channel, provider, worker_shutdown))?,
            );
        }
        Ok(Self { shutdown, threads })
    }

    pub(super) fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            if thread.join().is_err() {
                tracing::error!("notification delivery worker panicked");
            }
        }
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

fn run_worker(
    store: Store,
    channel: Channel,
    provider: Box<dyn Provider>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Acquire) {
        let now_ms = unix_time_ms();
        match store.claim_due(channel, now_ms) {
            Ok(Some(delivery)) => {
                let outcome = provider.deliver(&delivery);
                if let Err(error) = store.finish_delivery(&delivery, outcome, unix_time_ms()) {
                    tracing::warn!(
                        channel = channel.as_str(),
                        error = %error,
                        "unable to persist notification delivery result"
                    );
                }
            }
            Ok(None) => std::thread::sleep(DELIVERY_IDLE_INTERVAL),
            Err(error) => {
                tracing::warn!(
                    channel = channel.as_str(),
                    error = %error,
                    "notification delivery worker could not claim an outbox item"
                );
                std::thread::sleep(DELIVERY_IDLE_INTERVAL);
            }
        }
    }
}

impl Store {
    pub(super) fn recover_interrupted_deliveries(&self) -> anyhow::Result<()> {
        pollster::block_on(async {
            self.connection
                .execute(
                    "UPDATE notification_outbox SET status = 'retrying'
                     WHERE status = 'delivering'",
                    (),
                )
                .await?;
            Ok(())
        })
    }

    pub(super) fn claim_due(
        &self,
        channel: Channel,
        now_ms: i64,
    ) -> anyhow::Result<Option<Delivery>> {
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = self.claim_due_in_transaction(channel, now_ms).await;
            finish_transaction(&self.connection, result).await
        })
    }

    async fn claim_due_in_transaction(
        &self,
        channel: Channel,
        now_ms: i64,
    ) -> anyhow::Result<Option<Delivery>> {
        loop {
            let mut rows = self
                .connection
                .query(
                    "SELECT o.id, o.logical_id, l.rule_id, o.stage, o.destination_json,
                            o.payload_json, o.replacement_key, o.attempt_count,
                            o.max_attempts, o.max_retry_interval_ms, l.attachment_path,
                            o.attachment_enabled, o.attachment_required,
                            o.max_attachment_bytes, o.expires_at_ms
                     FROM notification_outbox AS o
                     JOIN logical_notifications AS l ON l.id = o.logical_id
                     WHERE o.channel = ?1 AND o.status IN ('pending', 'retrying')
                       AND o.next_attempt_at_ms <= ?2
                     ORDER BY o.priority DESC, o.id
                     LIMIT 1",
                    turso::params![channel.as_str(), now_ms],
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Ok(None);
            };
            let id = row.get::<i64>(0)?;
            let logical_id = row.get::<String>(1)?;
            let rule_id = row.get::<String>(2)?;
            let stage = parse_stage(&row.get::<String>(3)?)?;
            let destination_json = row.get::<String>(4)?;
            let payload_json = row.get::<String>(5)?;
            let replacement_key = row.get::<String>(6)?;
            let attempt_count = from_i64(row.get(7)?, "outbox attempt count")?;
            let max_attempts = to_u32(row.get(8)?, "maximum attempts")?;
            let max_retry_interval_ms = from_i64(row.get(9)?, "maximum retry interval")?;
            let attachment_path = row.get::<Option<String>>(10)?;
            let attachment_enabled = row.get::<i64>(11)? != 0;
            let attachment_required = row.get::<i64>(12)? != 0;
            let max_attachment_bytes = from_i64(row.get(13)?, "maximum attachment bytes")?;
            let expires_at_ms = row.get::<i64>(14)?;
            drop(rows);
            if now_ms >= expires_at_ms {
                self.expire_outbox(id, &logical_id, &rule_id, stage, now_ms, "outbox_expired")
                    .await?;
                continue;
            }
            let attempt = u32::try_from(attempt_count.saturating_add(1))
                .map_err(|_| anyhow::anyhow!("outbox attempt count exceeds u32"))?;
            self.connection
                .execute(
                    "UPDATE notification_outbox
                     SET status = 'delivering', attempt_count = ?2, updated_at_ms = ?3
                     WHERE id = ?1",
                    turso::params![id, i64::from(attempt), now_ms],
                )
                .await?;
            return Ok(Some(Delivery {
                id,
                logical_id,
                rule_id,
                stage,
                channel,
                destination_json,
                payload_json,
                replacement_key,
                attempt,
                max_attempts,
                max_retry_interval_ms,
                attachment_path,
                attachment_enabled,
                attachment_required,
                max_attachment_bytes,
                expires_at_ms,
            }));
        }
    }

    pub(super) fn finish_delivery(
        &self,
        delivery: &Delivery,
        outcome: DeliveryOutcome,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = self
                .finish_delivery_in_transaction(delivery, outcome, now_ms)
                .await;
            finish_transaction(&self.connection, result).await
        })
    }

    async fn finish_delivery_in_transaction(
        &self,
        delivery: &Delivery,
        outcome: DeliveryOutcome,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let target_hash = target_hash(&delivery.destination_json);
        let (status, history_outcome, reason, provider_status, retry_at_ms) = match outcome {
            DeliveryOutcome::Delivered { provider_status } => {
                ("delivered", "delivered", None, provider_status, None)
            }
            DeliveryOutcome::Permanent {
                reason,
                provider_status,
            } => (
                "failed",
                "failed",
                Some(redact_reason(reason)),
                provider_status,
                None,
            ),
            DeliveryOutcome::Transient {
                reason,
                provider_status,
                retry_after_ms,
            } => {
                let retry_interval_ms = retry_after_ms
                    .unwrap_or_else(|| retry_interval(delivery.attempt))
                    .clamp(MIN_RETRY_INTERVAL_MS, delivery.max_retry_interval_ms);
                let retry_at_ms = add_millis(now_ms, retry_interval_ms);
                if delivery.attempt >= delivery.max_attempts {
                    (
                        "failed",
                        "failed",
                        Some(redact_reason(reason)),
                        provider_status,
                        None,
                    )
                } else if retry_at_ms >= delivery.expires_at_ms {
                    (
                        "expired",
                        "expired",
                        Some("retry_exceeds_expiry".to_owned()),
                        provider_status,
                        None,
                    )
                } else {
                    (
                        "retrying",
                        "retried",
                        Some(redact_reason(reason)),
                        provider_status,
                        Some(retry_at_ms),
                    )
                }
            }
        };
        self.connection
            .execute(
                "UPDATE notification_outbox
                 SET status = ?2, next_attempt_at_ms = COALESCE(?3, next_attempt_at_ms),
                     updated_at_ms = ?4, last_reason = ?5
                 WHERE id = ?1 AND status = 'delivering'",
                turso::params![delivery.id, status, retry_at_ms, now_ms, reason.clone()],
            )
            .await?;
        self.connection
            .execute(
                "INSERT INTO notification_attempts (
                     outbox_id, logical_id, channel, stage, attempt, outcome,
                     target_hash, provider_status, reason, attempted_at_ms, retry_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                turso::params![
                    delivery.id,
                    delivery.logical_id.clone(),
                    delivery.channel.as_str(),
                    stage_str(delivery.stage),
                    i64::from(delivery.attempt),
                    history_outcome,
                    target_hash,
                    provider_status.map(i64::from),
                    reason.clone(),
                    now_ms,
                    retry_at_ms,
                ],
            )
            .await?;
        self.record_delivery_history(
            delivery,
            history_outcome,
            reason.as_deref(),
            now_ms,
            retry_at_ms,
        )
        .await?;
        if status == "delivered" {
            self.connection
                .execute(
                    "UPDATE notification_rules SET last_delivery_at_ms = ?2 WHERE id = ?1",
                    turso::params![delivery.rule_id.clone(), now_ms],
                )
                .await?;
        }
        Ok(())
    }

    async fn expire_outbox(
        &self,
        id: i64,
        logical_id: &str,
        rule_id: &str,
        stage: Stage,
        now_ms: i64,
        reason: &str,
    ) -> anyhow::Result<()> {
        self.connection
            .execute(
                "UPDATE notification_outbox
                 SET status = 'expired', updated_at_ms = ?2, last_reason = ?3
                 WHERE id = ?1",
                turso::params![id, now_ms, reason],
            )
            .await?;
        self.connection
            .execute(
                "INSERT INTO notification_history (
                     logical_id, rule_id, transition_revision, stage, outcome,
                     reason, occurred_at_ms
                 ) SELECT id, rule_id, highest_revision, ?3, 'expired', ?4, ?5
                   FROM logical_notifications WHERE id = ?1 AND rule_id = ?2",
                turso::params![logical_id, rule_id, stage_str(stage), reason, now_ms],
            )
            .await?;
        Ok(())
    }

    async fn record_delivery_history(
        &self,
        delivery: &Delivery,
        outcome: &str,
        reason: Option<&str>,
        now_ms: i64,
        retry_at_ms: Option<i64>,
    ) -> anyhow::Result<()> {
        self.connection
            .execute(
                "INSERT INTO notification_history (
                     logical_id, rule_id, transition_revision, stage, outcome,
                     reason, occurred_at_ms, next_eligible_at_ms
                 ) SELECT id, rule_id, highest_revision, ?3, ?4, ?5, ?6, ?7
                   FROM logical_notifications WHERE id = ?1 AND rule_id = ?2",
                turso::params![
                    delivery.logical_id.clone(),
                    delivery.rule_id.clone(),
                    stage_str(delivery.stage),
                    outcome,
                    reason,
                    now_ms,
                    retry_at_ms,
                ],
            )
            .await?;
        Ok(())
    }
}

async fn finish_transaction<T>(
    connection: &turso::Connection,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    match result {
        Ok(value) => {
            connection.execute_batch("COMMIT").await?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

fn retry_after_ms(headers: &ureq::http::HeaderMap) -> Option<u64> {
    headers
        .get(ureq::http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.saturating_mul(1_000))
}

fn webhook_payload(delivery: &Delivery) -> Result<Vec<u8>, &'static str> {
    let mut payload: WebhookPayload =
        serde_json::from_str(&delivery.payload_json).map_err(|_| "payload_invalid")?;
    if delivery.attachment_enabled {
        let attachment = delivery
            .attachment_path
            .as_deref()
            .and_then(|path| read_bounded_attachment(path, delivery.max_attachment_bytes));
        if let Some(bytes) = attachment {
            payload.attachment_content_type = Some("image/jpeg".to_owned());
            payload.attachment_base64 = Some(STANDARD.encode(bytes));
        } else if delivery.attachment_required {
            return Err("attachment_unavailable");
        }
    }
    serde_json::to_vec(&payload).map_err(|_| "payload_invalid")
}

fn read_bounded_attachment(path: &str, maximum_bytes: u64) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return None;
    }
    let capacity = usize::try_from(metadata.len()).ok()?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).ok()? <= maximum_bytes).then_some(bytes)
}

fn retry_interval(attempt: u32) -> u64 {
    let exponent = attempt.saturating_sub(1).min(20);
    MIN_RETRY_INTERVAL_MS.saturating_mul(1_u64 << exponent)
}

fn redact_reason(reason: String) -> String {
    let mut reason = reason;
    reason.truncate(MAX_REASON_BYTES);
    reason
}

fn target_hash(destination_json: &str) -> String {
    let digest = Sha256::digest(destination_json.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn unix_time_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

const fn stage_str(stage: Stage) -> &'static str {
    match stage {
        Stage::Preliminary => "preliminary",
        Stage::Enriched => "enriched",
        Stage::Recovery => "recovery",
    }
}

fn parse_stage(value: &str) -> anyhow::Result<Stage> {
    match value {
        "preliminary" => Ok(Stage::Preliminary),
        "enriched" => Ok(Stage::Enriched),
        "recovery" => Ok(Stage::Recovery),
        _ => anyhow::bail!("stored notification stage is invalid"),
    }
}

fn add_millis(timestamp_ms: i64, duration_ms: u64) -> i64 {
    timestamp_ms.saturating_add(i64::try_from(duration_ms).unwrap_or(i64::MAX))
}

fn from_i64(value: i64, name: &str) -> anyhow::Result<u64> {
    u64::try_from(value).map_err(|_| anyhow::anyhow!("stored {name} is negative"))
}

fn to_u32(value: i64, name: &str) -> anyhow::Result<u32> {
    u32::try_from(value).map_err(|_| anyhow::anyhow!("stored {name} is out of range"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("keeppeek-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn seed_outbox(
        store: &Store,
        logical_id: &str,
        channel: Channel,
        max_attempts: u32,
        max_retry_interval_ms: u64,
        expires_at_ms: i64,
    ) {
        pollster::block_on(async {
            store
                .connection
                .execute(
                    "INSERT INTO logical_notifications (
                         id, rule_id, owner_id, source_id, source_identity, lifecycle,
                         stage, highest_revision, created_at_ms, updated_at_ms,
                         enrichment_deadline_at_ms, title, body, deep_link, severity
                     ) VALUES (?1, 'rule-1', 'owner-1', 'front-door', 'event-1',
                               'event', 'preliminary', 1, 1000, 1000, 11000,
                               'Person', 'Detected', '/events/event-1', 'info')",
                    turso::params![logical_id],
                )
                .await
                .unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO notification_outbox (
                         logical_id, action_index, stage, channel, destination_json,
                         payload_json, replacement_key, priority, status, attempt_count,
                         max_attempts, max_retry_interval_ms, next_attempt_at_ms,
                         expires_at_ms, created_at_ms, updated_at_ms
                     ) VALUES (?1, 0, 'preliminary', ?2,
                               '{\"value\":\"https://secret.example/target\"}',
                               '{\"title\":\"Person\"}', ?1, 0, 'pending', 0,
                               ?3, ?4, 1000, ?5, 1000, 1000)",
                    turso::params![
                        logical_id,
                        channel.as_str(),
                        i64::from(max_attempts),
                        i64::try_from(max_retry_interval_ms).unwrap(),
                        expires_at_ms,
                    ],
                )
                .await
                .unwrap();
        });
    }

    fn text_value(store: &Store, sql: &str) -> String {
        pollster::block_on(async {
            let mut rows = store.connection.query(sql, ()).await.unwrap();
            rows.next().await.unwrap().unwrap().get(0).unwrap()
        })
    }

    fn count(store: &Store, sql: &str) -> u64 {
        pollster::block_on(async {
            let mut rows = store.connection.query(sql, ()).await.unwrap();
            let value = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
            u64::try_from(value).unwrap()
        })
    }

    #[test]
    fn successful_delivery_is_durable_and_target_is_hashed() {
        let directory = test_dir("notification-delivery-success");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
        seed_outbox(&store, "logical-1", Channel::Browser, 3, 5_000, 10_000);

        let delivery = store.claim_due(Channel::Browser, 1_000).unwrap().unwrap();
        assert_eq!(delivery.attempt, 1);
        store
            .finish_delivery(
                &delivery,
                DeliveryOutcome::Delivered {
                    provider_status: None,
                },
                1_100,
            )
            .unwrap();

        assert_eq!(
            text_value(
                &store,
                "SELECT status FROM notification_outbox WHERE logical_id = 'logical-1'"
            ),
            "delivered"
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM notification_history WHERE outcome = 'delivered'"
            ),
            1
        );
        let target_hash = text_value(&store, "SELECT target_hash FROM notification_attempts");
        assert_eq!(target_hash.len(), 64);
        assert!(!target_hash.contains("secret.example"));
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn transient_delivery_respects_retry_after_and_max_attempts() {
        let directory = test_dir("notification-delivery-retry");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
        seed_outbox(&store, "logical-1", Channel::Webhook, 2, 5_000, 20_000);

        let first = store.claim_due(Channel::Webhook, 1_000).unwrap().unwrap();
        store
            .finish_delivery(
                &first,
                DeliveryOutcome::Transient {
                    reason: "provider_busy".to_owned(),
                    provider_status: Some(429),
                    retry_after_ms: Some(2_000),
                },
                1_000,
            )
            .unwrap();
        assert!(store.claim_due(Channel::Webhook, 2_999).unwrap().is_none());

        let second = store.claim_due(Channel::Webhook, 3_000).unwrap().unwrap();
        assert_eq!(second.attempt, 2);
        store
            .finish_delivery(
                &second,
                DeliveryOutcome::Transient {
                    reason: "provider_busy".to_owned(),
                    provider_status: Some(429),
                    retry_after_ms: Some(2_000),
                },
                3_000,
            )
            .unwrap();
        assert_eq!(
            text_value(&store, "SELECT status FROM notification_outbox"),
            "failed"
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM notification_attempts WHERE outcome IN ('retried', 'failed')"
            ),
            2
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn expired_and_unrelated_channel_items_do_not_block_claiming() {
        let directory = test_dir("notification-delivery-expiry");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
        seed_outbox(&store, "logical-webhook", Channel::Webhook, 2, 5_000, 1_500);
        seed_outbox(
            &store,
            "logical-browser",
            Channel::Browser,
            2,
            5_000,
            10_000,
        );

        assert!(store.claim_due(Channel::Webhook, 1_500).unwrap().is_none());
        assert_eq!(
            text_value(
                &store,
                "SELECT status FROM notification_outbox WHERE logical_id = 'logical-webhook'"
            ),
            "expired"
        );
        let browser = store.claim_due(Channel::Browser, 1_500).unwrap().unwrap();
        assert_eq!(browser.logical_id, "logical-browser");
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn permanent_provider_failure_is_not_retried() {
        let directory = test_dir("notification-delivery-permanent");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
        seed_outbox(&store, "logical-1", Channel::Push, 4, 5_000, 20_000);

        let delivery = store.claim_due(Channel::Push, 1_000).unwrap().unwrap();
        store
            .finish_delivery(
                &delivery,
                DeliveryOutcome::Permanent {
                    reason: "channel_unavailable".to_owned(),
                    provider_status: None,
                },
                1_100,
            )
            .unwrap();
        assert_eq!(
            text_value(&store, "SELECT status FROM notification_outbox"),
            "failed"
        );
        assert!(store.claim_due(Channel::Push, 10_000).unwrap().is_none());
        assert_eq!(
            text_value(&store, "SELECT reason FROM notification_attempts"),
            "channel_unavailable"
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn webhook_payload_reads_bounded_bytes_without_serializing_the_path() {
        let directory = test_dir("notification-webhook-attachment");
        let image_path = directory.join("event-1.jpg");
        std::fs::write(&image_path, [1_u8, 2, 3, 4]).unwrap();
        let mut delivery = Delivery {
            id: 1,
            logical_id: "logical-1".to_owned(),
            rule_id: "rule-1".to_owned(),
            stage: Stage::Enriched,
            channel: Channel::Webhook,
            destination_json: r#"{"value":"https://example.invalid"}"#.to_owned(),
            payload_json: r#"{"title":"Person","body":"Detected","deep_link":"/events"}"#
                .to_owned(),
            replacement_key: "logical-1".to_owned(),
            attempt: 1,
            max_attempts: 3,
            max_retry_interval_ms: 5_000,
            attachment_path: Some(image_path.to_string_lossy().into_owned()),
            attachment_enabled: true,
            attachment_required: true,
            max_attachment_bytes: 4,
            expires_at_ms: 10_000,
        };
        let payload = String::from_utf8(webhook_payload(&delivery).unwrap()).unwrap();
        assert!(payload.contains("AQIDBA=="));
        assert!(!payload.contains(image_path.to_string_lossy().as_ref()));

        std::fs::write(&image_path, [1_u8, 2, 3, 4, 5]).unwrap();
        assert_eq!(webhook_payload(&delivery), Err("attachment_unavailable"));
        delivery.attachment_required = false;
        let fallback = String::from_utf8(webhook_payload(&delivery).unwrap()).unwrap();
        assert!(!fallback.contains("attachment_base64"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
