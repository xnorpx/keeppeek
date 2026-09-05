use std::{
    fs::File,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    Stage, decrement_counter,
    model::Channel,
    pushover,
    state::{DeliveryAttempt, OutboxStatus, PendingHistoryEntry, RuntimeState},
    store::Store,
};
use crate::storage::metadata::EventAttachment;

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(5);
const PUSHOVER_MESSAGES_URL: &str = "https://api.pushover.net/1/messages.json";
const PUSHOVER_RECEIPTS_URL: &str = "https://api.pushover.net/1/receipts/";
const PUSHOVER_TIMEOUT: Duration = Duration::from_secs(5);
const PUSHOVER_MIN_RETRY_INTERVAL_MS: u64 = 5_000;
const DELIVERY_IDLE_INTERVAL: Duration = Duration::from_millis(100);
const MIN_RETRY_INTERVAL_MS: u64 = 1_000;
const MAX_REASON_BYTES: usize = 256;
const MAX_PROVIDER_RESPONSE_BYTES: u64 = 64 * 1_024;

#[derive(Debug, Clone)]
pub(super) struct Delivery {
    id: u64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProviderResult {
    outcome: DeliveryOutcome,
    request_id: Option<String>,
    receipt: Option<ProviderReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderReceipt {
    id: String,
    expire_seconds: u16,
}

#[derive(Debug, Clone)]
struct ReceiptCheck {
    outbox_id: u64,
    logical_id: String,
    rule_id: String,
    stage: Stage,
    destination_json: String,
    receipt: String,
    receipt_expires_at_ms: i64,
    max_retry_interval_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReceiptOutcome {
    Pending {
        expires_at_ms: Option<i64>,
    },
    Acknowledged {
        acknowledged_at_ms: i64,
        acknowledged_by_hash: Option<String>,
    },
    Expired {
        expired_at_ms: Option<i64>,
    },
    Transient {
        reason: String,
        retry_after_ms: Option<u64>,
    },
    Permanent {
        reason: String,
    },
}

struct DeliveryTransition {
    status: OutboxStatus,
    history_outcome: &'static str,
    reason: Option<String>,
    provider_status: Option<u16>,
    retry_at_ms: Option<i64>,
    provider_request_id: Option<String>,
    provider_receipt: Option<String>,
    next_receipt_check_at_ms: Option<i64>,
    provider_receipt_expires_at_ms: Option<i64>,
}

impl From<DeliveryOutcome> for ProviderResult {
    fn from(outcome: DeliveryOutcome) -> Self {
        Self {
            outcome,
            request_id: None,
            receipt: None,
        }
    }
}

trait Provider: Send + Sync {
    fn deliver(&self, delivery: &Delivery) -> ProviderResult;

    fn check_receipt(&self, _receipt: &ReceiptCheck) -> ReceiptOutcome {
        ReceiptOutcome::Permanent {
            reason: "receipt_unsupported".to_owned(),
        }
    }
}

struct BrowserProvider;

impl Provider for BrowserProvider {
    fn deliver(&self, _delivery: &Delivery) -> ProviderResult {
        DeliveryOutcome::Delivered {
            provider_status: None,
        }
        .into()
    }
}

struct UnavailableProvider;

impl Provider for UnavailableProvider {
    fn deliver(&self, _delivery: &Delivery) -> ProviderResult {
        DeliveryOutcome::Permanent {
            reason: "channel_unavailable".to_owned(),
            provider_status: None,
        }
        .into()
    }
}

struct PushoverProvider {
    agent: ureq::Agent,
    messages_url: String,
    receipts_url: String,
}

struct WebhookProvider {
    agent: ureq::Agent,
}

#[derive(Deserialize, Serialize)]
struct Destination {
    value: String,
}

#[derive(Deserialize, Serialize)]
struct ProviderPayload {
    title: String,
    body: String,
    deep_link: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    occurred_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    canonical_attachment: Option<EventAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_availability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attachment_content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attachment_base64: Option<String>,
}

#[derive(Serialize)]
struct PushoverMessage<'a> {
    token: &'a str,
    user: &'a str,
    message: &'a str,
    title: &'a str,
    priority: i8,
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sound: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expire: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url_title: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attachment_base64: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attachment_type: Option<&'static str>,
}

#[derive(Deserialize)]
struct PushoverResponse {
    status: u8,
    request: Option<String>,
    receipt: Option<String>,
}

#[derive(Deserialize)]
struct PushoverReceiptResponse {
    status: u8,
    request: Option<String>,
    acknowledged: u8,
    #[serde(default)]
    acknowledged_at: i64,
    acknowledged_by: Option<String>,
    expired: u8,
    #[serde(default)]
    expires_at: i64,
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
    fn deliver(&self, delivery: &Delivery) -> ProviderResult {
        let destination: Destination = match serde_json::from_str(&delivery.destination_json) {
            Ok(destination) => destination,
            Err(_) => {
                return DeliveryOutcome::Permanent {
                    reason: "destination_invalid".to_owned(),
                    provider_status: None,
                }
                .into();
            }
        };
        let payload = match webhook_payload(delivery) {
            Ok(payload) => payload,
            Err(reason) => {
                return DeliveryOutcome::Permanent {
                    reason: reason.to_owned(),
                    provider_status: None,
                }
                .into();
            }
        };
        let response = self
            .agent
            .post(&destination.value)
            .header("Content-Type", "application/json")
            .header("X-KeepPeek-Collapse-Key", &delivery.replacement_key)
            .header("X-KeepPeek-Stage", stage_str(delivery.stage))
            .send(payload.as_slice());
        let outcome = match response {
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
        };
        outcome.into()
    }
}

impl PushoverProvider {
    fn new() -> Self {
        Self::with_urls(PUSHOVER_MESSAGES_URL, PUSHOVER_RECEIPTS_URL)
    }

    #[cfg(test)]
    fn with_messages_url(messages_url: impl Into<String>) -> Self {
        Self::with_urls(messages_url, PUSHOVER_RECEIPTS_URL)
    }

    fn with_urls(messages_url: impl Into<String>, receipts_url: impl Into<String>) -> Self {
        Self::with_urls_and_timeout(messages_url, receipts_url, PUSHOVER_TIMEOUT)
    }

    fn with_urls_and_timeout(
        messages_url: impl Into<String>,
        receipts_url: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(timeout))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            messages_url: messages_url.into(),
            receipts_url: receipts_url.into(),
        }
    }
}

impl Provider for PushoverProvider {
    fn deliver(&self, delivery: &Delivery) -> ProviderResult {
        let destination: Destination = match serde_json::from_str(&delivery.destination_json) {
            Ok(destination) => destination,
            Err(_) => return permanent("destination_invalid", None).into(),
        };
        let destination = match pushover::Destination::parse(&destination.value) {
            Ok(destination) => destination,
            Err(_) => return permanent("destination_invalid", None).into(),
        };
        let payload: ProviderPayload = match serde_json::from_str(&delivery.payload_json) {
            Ok(payload) => payload,
            Err(_) => return permanent("payload_invalid", None).into(),
        };
        let attachment = match provider_attachment(delivery) {
            Ok(attachment) => attachment,
            Err(reason) => return permanent(reason, None).into(),
        };
        let deep_link = match destination.deep_link(&payload.deep_link) {
            Ok(deep_link) => deep_link,
            Err(_) => return permanent("deep_link_invalid", None).into(),
        };
        let attachment_base64 = attachment.map(|bytes| STANDARD.encode(bytes));
        let message = PushoverMessage {
            token: &destination.application_token,
            user: &destination.user_key,
            message: &payload.body,
            title: &payload.title,
            priority: destination.priority,
            device: destination.device.as_deref(),
            sound: destination.sound.as_deref(),
            retry: destination.retry_seconds,
            expire: destination.expire_seconds,
            timestamp: payload.occurred_at_ms.map(|timestamp| timestamp / 1_000),
            url: deep_link.as_deref(),
            url_title: deep_link.as_ref().map(|_| "Open in KeepPeek"),
            attachment_base64: attachment_base64.as_deref(),
            attachment_type: attachment_base64.as_ref().map(|_| "image/jpeg"),
        };
        let body = match serde_json::to_vec(&message) {
            Ok(body) => body,
            Err(_) => return permanent("payload_invalid", None).into(),
        };
        let response = self
            .agent
            .post(&self.messages_url)
            .header("Content-Type", "application/json")
            .send(body.as_slice());
        match response {
            Ok(mut response) => {
                let status = response.status().as_u16();
                if !response.status().is_success() {
                    return classify_http_failure(status, response.headers()).into();
                }
                let response_body = match response
                    .body_mut()
                    .with_config()
                    .limit(MAX_PROVIDER_RESPONSE_BYTES)
                    .read_to_string()
                {
                    Ok(response_body) => response_body,
                    Err(_) => {
                        return transient("provider_response_invalid", Some(status), None).into();
                    }
                };
                let response: PushoverResponse = match serde_json::from_str(&response_body) {
                    Ok(response) => response,
                    Err(_) => {
                        return transient("provider_response_invalid", Some(status), None).into();
                    }
                };
                let Some(request_id) = response.request.filter(|value| valid_request_id(value))
                else {
                    return permanent("provider_response_invalid", Some(status)).into();
                };
                let receipt = if destination.priority == 2 {
                    let Some(receipt) = response.receipt.filter(|value| valid_receipt(value))
                    else {
                        return permanent("provider_response_invalid", Some(status)).into();
                    };
                    Some(ProviderReceipt {
                        id: receipt,
                        expire_seconds: destination.expire_seconds.unwrap_or_default(),
                    })
                } else {
                    None
                };
                if response.status != 1 {
                    return ProviderResult {
                        outcome: permanent("provider_rejected", Some(status)),
                        request_id: Some(request_id),
                        receipt: None,
                    };
                }
                ProviderResult {
                    outcome: DeliveryOutcome::Delivered {
                        provider_status: Some(status),
                    },
                    request_id: Some(request_id),
                    receipt,
                }
            }
            Err(ureq::Error::Timeout(_)) => transient(
                "provider_timeout",
                None,
                Some(PUSHOVER_MIN_RETRY_INTERVAL_MS),
            )
            .into(),
            Err(ureq::Error::HostNotFound | ureq::Error::ConnectionFailed) => transient(
                "provider_unreachable",
                None,
                Some(PUSHOVER_MIN_RETRY_INTERVAL_MS),
            )
            .into(),
            Err(_) => transient(
                "provider_transport_error",
                None,
                Some(PUSHOVER_MIN_RETRY_INTERVAL_MS),
            )
            .into(),
        }
    }

    fn check_receipt(&self, receipt: &ReceiptCheck) -> ReceiptOutcome {
        let destination: Destination = match serde_json::from_str(&receipt.destination_json) {
            Ok(destination) => destination,
            Err(_) => return receipt_permanent("destination_invalid"),
        };
        let destination = match pushover::Destination::parse(&destination.value) {
            Ok(destination) => destination,
            Err(_) => return receipt_permanent("destination_invalid"),
        };
        let url = match pushover_receipt_url(
            &self.receipts_url,
            &receipt.receipt,
            &destination.application_token,
        ) {
            Ok(url) => url,
            Err(_) => return receipt_permanent("receipt_invalid"),
        };
        let response = self.agent.get(url.as_str()).call();
        match response {
            Ok(mut response) => {
                let status = response.status().as_u16();
                if !response.status().is_success() {
                    return classify_receipt_http_failure(status, response.headers());
                }
                let response_body = match response
                    .body_mut()
                    .with_config()
                    .limit(MAX_PROVIDER_RESPONSE_BYTES)
                    .read_to_string()
                {
                    Ok(response_body) => response_body,
                    Err(_) => return receipt_transient("provider_response_invalid", None),
                };
                let response: PushoverReceiptResponse = match serde_json::from_str(&response_body) {
                    Ok(response) => response,
                    Err(_) => return receipt_transient("provider_response_invalid", None),
                };
                if response.status != 1
                    || response
                        .request
                        .as_deref()
                        .is_none_or(|value| !valid_request_id(value))
                    || response.acknowledged > 1
                    || response.expired > 1
                {
                    return receipt_permanent("provider_response_invalid");
                }
                if response.acknowledged == 1 {
                    let Some(acknowledged_at_ms) = seconds_to_millis(response.acknowledged_at)
                    else {
                        return receipt_permanent("provider_response_invalid");
                    };
                    return ReceiptOutcome::Acknowledged {
                        acknowledged_at_ms,
                        acknowledged_by_hash: response
                            .acknowledged_by
                            .as_deref()
                            .filter(|value| !value.is_empty())
                            .map(target_hash),
                    };
                }
                if response.expired == 1 {
                    return ReceiptOutcome::Expired {
                        expired_at_ms: seconds_to_millis(response.expires_at),
                    };
                }
                ReceiptOutcome::Pending {
                    expires_at_ms: seconds_to_millis(response.expires_at),
                }
            }
            Err(ureq::Error::Timeout(_)) => {
                receipt_transient("provider_timeout", Some(PUSHOVER_MIN_RETRY_INTERVAL_MS))
            }
            Err(ureq::Error::HostNotFound | ureq::Error::ConnectionFailed) => {
                receipt_transient("provider_unreachable", Some(PUSHOVER_MIN_RETRY_INTERVAL_MS))
            }
            Err(_) => receipt_transient(
                "provider_transport_error",
                Some(PUSHOVER_MIN_RETRY_INTERVAL_MS),
            ),
        }
    }
}

pub(super) struct Workers {
    shutdown: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Workers {
    pub(super) fn start(store: Store) -> anyhow::Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::with_capacity(4);
        let channels = [
            Channel::Browser,
            Channel::Push,
            Channel::Webhook,
            Channel::Forwarder,
        ];
        for channel in channels {
            let store = store.clone();
            let worker_shutdown = shutdown.clone();
            let provider: Box<dyn Provider> = match channel {
                Channel::Browser => Box::new(BrowserProvider),
                Channel::Push => Box::new(PushoverProvider::new()),
                Channel::Webhook => Box::new(WebhookProvider::new()),
                Channel::Forwarder => Box::new(UnavailableProvider),
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
                tracing::error!(
                    event = "notification_delivery_worker_panicked",
                    "notification delivery worker panicked"
                );
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
        let mut did_work = false;
        match store.claim_due(channel, now_ms) {
            Ok(Some(delivery)) => {
                did_work = true;
                let outcome = provider.deliver(&delivery);
                if let Err(error) = store.finish_delivery(&delivery, outcome, unix_time_ms()) {
                    tracing::warn!(
                        event = "notification_delivery_record_failed",
                        channel = channel.as_str(),
                        error = %error,
                        "unable to record notification delivery result"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    event = "notification_delivery_claim_failed",
                    channel = channel.as_str(),
                    error = %error,
                    "notification delivery worker could not claim an outbox item"
                );
            }
        }
        if channel == Channel::Push {
            match store.claim_due_receipt(now_ms) {
                Ok(Some(receipt)) => {
                    did_work = true;
                    let outcome = provider.check_receipt(&receipt);
                    if let Err(error) = store.finish_receipt(&receipt, outcome, unix_time_ms()) {
                        tracing::warn!(
                            event = "notification_receipt_record_failed",
                            channel = channel.as_str(),
                            error = %error,
                            "unable to record notification receipt result"
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        event = "notification_receipt_claim_failed",
                        channel = channel.as_str(),
                        error = %error,
                        "notification delivery worker could not claim a receipt check"
                    );
                }
            }
        }
        if !did_work {
            std::thread::sleep(DELIVERY_IDLE_INTERVAL);
        }
    }
}

impl Store {
    pub(super) fn claim_due(
        &self,
        channel: Channel,
        now_ms: i64,
    ) -> anyhow::Result<Option<Delivery>> {
        let (delivery, expired_count) = {
            let mut state = self.lock_state();
            let mut expired_count = 0_u64;
            let delivery = loop {
                let Some(id) = Self::next_due_outbox_id(&state, channel, now_ms) else {
                    break None;
                };
                let item = state
                    .outbox
                    .get(&id)
                    .cloned()
                    .expect("a selected outbox item must exist");
                let logical = state
                    .logical
                    .get(&item.logical_id)
                    .cloned()
                    .expect("a selected outbox item must reference a logical notification");
                if now_ms >= item.expires_at_ms {
                    Self::expire_due_outbox(&mut state, id, now_ms);
                    expired_count = expired_count.saturating_add(1);
                    continue;
                }
                let attempt = item
                    .attempt_count
                    .checked_add(1)
                    .expect("a bounded outbox attempt count must not overflow");
                let claimed = state
                    .outbox
                    .get_mut(&id)
                    .expect("a selected outbox item must exist");
                claimed.status = OutboxStatus::Delivering;
                claimed.attempt_count = attempt;
                claimed.updated_at_ms = now_ms;
                break Some(Delivery {
                    id,
                    logical_id: item.logical_id,
                    rule_id: logical.rule_id,
                    stage: item.stage,
                    channel,
                    destination_json: item.destination_json,
                    payload_json: item.payload_json,
                    replacement_key: item.replacement_key,
                    attempt,
                    max_attempts: item.max_attempts,
                    max_retry_interval_ms: item.max_retry_interval_ms,
                    attachment_path: logical.attachment_path,
                    attachment_enabled: item.attachment_enabled,
                    attachment_required: item.attachment_required,
                    max_attachment_bytes: item.max_attachment_bytes,
                    expires_at_ms: item.expires_at_ms,
                });
            };
            (delivery, expired_count)
        };
        if expired_count != 0 {
            decrement_counter(&self.metrics.pending_deliveries, expired_count);
            self.metrics
                .delivery_failures
                .fetch_add(expired_count, Ordering::Relaxed);
        }
        Ok(delivery)
    }

    fn claim_due_receipt(&self, now_ms: i64) -> anyhow::Result<Option<ReceiptCheck>> {
        let mut state = self.lock_state();
        let Some(id) = state
            .outbox
            .values()
            .filter(|item| {
                item.channel == Channel::Push
                    && item.receipt_pending()
                    && item
                        .next_receipt_check_at_ms
                        .is_some_and(|next_check_at_ms| next_check_at_ms <= now_ms)
                    && state.logical.contains_key(&item.logical_id)
            })
            .min_by_key(|item| (item.next_receipt_check_at_ms, item.id))
            .map(|item| item.id)
        else {
            return Ok(None);
        };
        let item = state
            .outbox
            .get(&id)
            .cloned()
            .expect("a selected receipt outbox item must exist");
        let logical = state
            .logical
            .get(&item.logical_id)
            .expect("a selected receipt must reference a logical notification");
        let receipt = ReceiptCheck {
            outbox_id: id,
            logical_id: item.logical_id.clone(),
            rule_id: logical.rule_id.clone(),
            stage: item.stage,
            destination_json: item.destination_json,
            receipt: item
                .provider_receipt
                .expect("a receipt-pending outbox item must have a provider receipt"),
            receipt_expires_at_ms: item
                .provider_receipt_expires_at_ms
                .expect("a receipt-pending outbox item must have a provider receipt expiration"),
            max_retry_interval_ms: item.max_retry_interval_ms,
        };
        let claimed = state
            .outbox
            .get_mut(&id)
            .expect("a selected receipt outbox item must exist");
        claimed.next_receipt_check_at_ms = Some(add_millis(now_ms, PUSHOVER_MIN_RETRY_INTERVAL_MS));
        claimed.updated_at_ms = now_ms;
        Ok(Some(receipt))
    }

    fn next_due_outbox_id(state: &RuntimeState, channel: Channel, now_ms: i64) -> Option<u64> {
        state
            .outbox
            .values()
            .filter(|item| {
                item.channel == channel
                    && item.status.pending()
                    && item.next_attempt_at_ms <= now_ms
                    && state.logical.contains_key(&item.logical_id)
            })
            .max_by(|left, right| {
                left.priority
                    .cmp(&right.priority)
                    .then_with(|| right.id.cmp(&left.id))
            })
            .map(|item| item.id)
    }

    fn expire_due_outbox(state: &mut RuntimeState, id: u64, now_ms: i64) {
        let item = state
            .outbox
            .get_mut(&id)
            .expect("a selected outbox item must exist");
        item.status = OutboxStatus::Expired;
        item.updated_at_ms = now_ms;
        item.last_reason = Some("outbox_expired".to_owned());
        let logical_id = item.logical_id.clone();
        let stage = item.stage;
        let logical = state
            .logical
            .get(&logical_id)
            .expect("an outbox item must reference a logical notification");
        let rule_id = logical.rule_id.clone();
        let revision = logical.highest_revision;
        state.push_history(PendingHistoryEntry {
            logical_id: &logical_id,
            rule_id: &rule_id,
            revision,
            stage,
            outcome: "expired",
            reason: Some("outbox_expired"),
            occurred_at_ms: now_ms,
            next_eligible_at_ms: None,
        });
    }

    pub(super) fn finish_delivery(
        &self,
        delivery: &Delivery,
        result: impl Into<ProviderResult>,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let target_hash = target_hash(&delivery.destination_json);
        let transition = Self::delivery_transition(delivery, result.into(), now_ms);
        {
            let mut state = self.lock_state();
            Self::validate_delivery_claim(&state, delivery)?;
            Self::apply_delivery_transition(
                &mut state,
                delivery,
                &transition,
                &target_hash,
                now_ms,
            );
            state.prune(now_ms)?;
        }
        self.record_delivery_metrics(transition.status);
        tracing::info!(
            event = "notification_delivery_finished",
            logical_id = %delivery.logical_id,
            rule_id = %delivery.rule_id,
            channel = delivery.channel.as_str(),
            target_hash,
            attempt = delivery.attempt,
            outcome = transition.history_outcome,
            reason = transition.reason.as_deref().unwrap_or(""),
            provider_status = transition.provider_status
        );
        Ok(())
    }

    fn validate_delivery_claim(state: &RuntimeState, delivery: &Delivery) -> anyhow::Result<()> {
        let item = state
            .outbox
            .get(&delivery.id)
            .ok_or_else(|| anyhow::anyhow!("claimed notification delivery no longer exists"))?;
        let valid_item = item.status == OutboxStatus::Delivering
            && item.attempt_count == delivery.attempt
            && item.logical_id == delivery.logical_id
            && item.channel == delivery.channel
            && item.stage == delivery.stage;
        let valid_logical = state
            .logical
            .get(&delivery.logical_id)
            .is_some_and(|logical| logical.rule_id == delivery.rule_id);
        if !valid_item {
            anyhow::bail!("notification delivery claim is stale");
        }
        if !valid_logical {
            anyhow::bail!("notification delivery references invalid logical state");
        }
        Ok(())
    }

    fn apply_delivery_transition(
        state: &mut RuntimeState,
        delivery: &Delivery,
        transition: &DeliveryTransition,
        target_hash: &str,
        now_ms: i64,
    ) {
        let item = state
            .outbox
            .get_mut(&delivery.id)
            .expect("a validated outbox item must exist");
        item.status = transition.status;
        if let Some(retry_at_ms) = transition.retry_at_ms {
            item.next_attempt_at_ms = retry_at_ms;
        }
        item.updated_at_ms = now_ms;
        item.last_reason = transition.reason.clone();
        item.provider_request_id = transition.provider_request_id.clone();
        item.provider_receipt = transition.provider_receipt.clone();
        item.next_receipt_check_at_ms = transition.next_receipt_check_at_ms;
        item.provider_receipt_expires_at_ms = transition.provider_receipt_expires_at_ms;
        state.push_attempt(DeliveryAttempt {
            sequence: 0,
            outbox_id: delivery.id,
            logical_id: delivery.logical_id.clone(),
            channel: delivery.channel,
            stage: delivery.stage,
            attempt: delivery.attempt,
            outcome: transition.history_outcome.to_owned(),
            target_hash: target_hash.to_owned(),
            provider_status: transition.provider_status,
            provider_request_id: transition.provider_request_id.clone(),
            reason: transition.reason.clone(),
            attempted_at_ms: now_ms,
            retry_at_ms: transition.retry_at_ms,
        });
        Self::push_delivery_history(state, delivery, transition, now_ms);
        if transition.status == OutboxStatus::Delivered
            && let Some(record) = state.rules.get_mut(&delivery.rule_id)
        {
            record.last_delivery_at_ms = Some(now_ms);
        }
    }

    fn delivery_transition(
        delivery: &Delivery,
        result: ProviderResult,
        now_ms: i64,
    ) -> DeliveryTransition {
        let ProviderResult {
            outcome,
            request_id,
            receipt,
        } = result;
        let mut transition = match outcome {
            DeliveryOutcome::Delivered { provider_status } => Self::new_delivery_transition(
                OutboxStatus::Delivered,
                "delivered",
                None,
                provider_status,
                None,
            ),
            DeliveryOutcome::Permanent {
                reason,
                provider_status,
            } => Self::new_delivery_transition(
                OutboxStatus::Failed,
                "failed",
                Some(redact_reason(reason)),
                provider_status,
                None,
            ),
            DeliveryOutcome::Transient {
                reason,
                provider_status,
                retry_after_ms,
            } => Self::transient_delivery_transition(
                delivery,
                reason,
                provider_status,
                retry_after_ms,
                now_ms,
            ),
        };
        transition.provider_request_id = request_id;
        if transition.status == OutboxStatus::Delivered
            && let Some(receipt) = receipt
        {
            transition.provider_receipt = Some(receipt.id);
            transition.next_receipt_check_at_ms =
                Some(add_millis(now_ms, PUSHOVER_MIN_RETRY_INTERVAL_MS));
            transition.provider_receipt_expires_at_ms = Some(add_millis(
                now_ms,
                u64::from(receipt.expire_seconds).saturating_mul(1_000),
            ));
        }
        transition
    }

    fn transient_delivery_transition(
        delivery: &Delivery,
        reason: String,
        provider_status: Option<u16>,
        retry_after_ms: Option<u64>,
        now_ms: i64,
    ) -> DeliveryTransition {
        let retry_interval_ms = retry_after_ms
            .unwrap_or_else(|| retry_interval(delivery.attempt))
            .max(MIN_RETRY_INTERVAL_MS)
            .min(delivery.max_retry_interval_ms);
        let retry_at_ms = add_millis(now_ms, retry_interval_ms);
        if delivery.attempt >= delivery.max_attempts {
            return Self::new_delivery_transition(
                OutboxStatus::Failed,
                "failed",
                Some(redact_reason(reason)),
                provider_status,
                None,
            );
        }
        if retry_at_ms >= delivery.expires_at_ms {
            return Self::new_delivery_transition(
                OutboxStatus::Expired,
                "expired",
                Some("retry_exceeds_expiry".to_owned()),
                provider_status,
                None,
            );
        }
        Self::new_delivery_transition(
            OutboxStatus::Retrying,
            "retried",
            Some(redact_reason(reason)),
            provider_status,
            Some(retry_at_ms),
        )
    }

    const fn new_delivery_transition(
        status: OutboxStatus,
        history_outcome: &'static str,
        reason: Option<String>,
        provider_status: Option<u16>,
        retry_at_ms: Option<i64>,
    ) -> DeliveryTransition {
        DeliveryTransition {
            status,
            history_outcome,
            reason,
            provider_status,
            retry_at_ms,
            provider_request_id: None,
            provider_receipt: None,
            next_receipt_check_at_ms: None,
            provider_receipt_expires_at_ms: None,
        }
    }

    fn push_delivery_history(
        state: &mut RuntimeState,
        delivery: &Delivery,
        transition: &DeliveryTransition,
        now_ms: i64,
    ) {
        let logical = state
            .logical
            .get(&delivery.logical_id)
            .expect("a validated delivery must reference a logical notification");
        let revision = logical.highest_revision;
        state.push_history(PendingHistoryEntry {
            logical_id: &delivery.logical_id,
            rule_id: &delivery.rule_id,
            revision,
            stage: delivery.stage,
            outcome: transition.history_outcome,
            reason: transition.reason.as_deref(),
            occurred_at_ms: now_ms,
            next_eligible_at_ms: transition.retry_at_ms,
        });
    }

    fn record_delivery_metrics(&self, status: OutboxStatus) {
        self.metrics
            .delivery_attempts
            .fetch_add(1, Ordering::Relaxed);
        match status {
            OutboxStatus::Delivered => {
                decrement_counter(&self.metrics.pending_deliveries, 1);
                self.metrics
                    .delivery_successes
                    .fetch_add(1, Ordering::Relaxed);
            }
            OutboxStatus::Retrying => {
                self.metrics
                    .delivery_retries
                    .fetch_add(1, Ordering::Relaxed);
            }
            OutboxStatus::Failed | OutboxStatus::Expired => {
                decrement_counter(&self.metrics.pending_deliveries, 1);
                self.metrics
                    .delivery_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
            OutboxStatus::Pending | OutboxStatus::Delivering => {
                unreachable!("delivery outcome produced a non-terminal transition: {status:?}")
            }
        }
    }

    fn finish_receipt(
        &self,
        receipt: &ReceiptCheck,
        outcome: ReceiptOutcome,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let mut state = self.lock_state();
        Self::validate_receipt_claim(&state, receipt)?;
        Self::apply_receipt_outcome(&mut state, receipt, outcome, now_ms);
        state.prune(now_ms)
    }

    fn validate_receipt_claim(state: &RuntimeState, receipt: &ReceiptCheck) -> anyhow::Result<()> {
        let item = state
            .outbox
            .get(&receipt.outbox_id)
            .ok_or_else(|| anyhow::anyhow!("claimed notification receipt no longer exists"))?;
        let valid_item = item.status == OutboxStatus::Delivered
            && item.channel == Channel::Push
            && item.logical_id == receipt.logical_id
            && item.stage == receipt.stage
            && item.provider_receipt.as_deref() == Some(receipt.receipt.as_str())
            && item.provider_acknowledged_at_ms.is_none()
            && item.provider_expired_at_ms.is_none();
        let valid_logical = state
            .logical
            .get(&receipt.logical_id)
            .is_some_and(|logical| logical.rule_id == receipt.rule_id);
        if !valid_item || !valid_logical {
            anyhow::bail!("notification receipt claim is stale");
        }
        Ok(())
    }

    fn apply_receipt_outcome(
        state: &mut RuntimeState,
        receipt: &ReceiptCheck,
        outcome: ReceiptOutcome,
        now_ms: i64,
    ) {
        match outcome {
            ReceiptOutcome::Pending { expires_at_ms } => {
                Self::finish_pending_receipt(state, receipt, expires_at_ms, now_ms);
            }
            ReceiptOutcome::Acknowledged {
                acknowledged_at_ms,
                acknowledged_by_hash,
            } => {
                Self::acknowledge_receipt(
                    state,
                    receipt,
                    acknowledged_at_ms,
                    acknowledged_by_hash,
                    now_ms,
                );
            }
            ReceiptOutcome::Expired { expired_at_ms } => {
                Self::expire_receipt(
                    state,
                    receipt,
                    expired_at_ms.unwrap_or(now_ms),
                    now_ms,
                    "provider_unacknowledged",
                );
            }
            ReceiptOutcome::Transient {
                reason,
                retry_after_ms,
            } => {
                Self::retry_receipt(state, receipt, reason, retry_after_ms, now_ms);
            }
            ReceiptOutcome::Permanent { reason } => {
                Self::fail_receipt(state, receipt, reason, now_ms);
            }
        }
    }

    fn finish_pending_receipt(
        state: &mut RuntimeState,
        receipt: &ReceiptCheck,
        expires_at_ms: Option<i64>,
        now_ms: i64,
    ) {
        let expires_at_ms = expires_at_ms.unwrap_or(receipt.receipt_expires_at_ms);
        if now_ms >= expires_at_ms {
            Self::expire_receipt(
                state,
                receipt,
                expires_at_ms,
                now_ms,
                "provider_unacknowledged",
            );
            return;
        }
        let item = state
            .outbox
            .get_mut(&receipt.outbox_id)
            .expect("a validated receipt outbox item must exist");
        item.next_receipt_check_at_ms = Some(add_millis(now_ms, PUSHOVER_MIN_RETRY_INTERVAL_MS));
        item.provider_receipt_expires_at_ms = Some(expires_at_ms);
        item.updated_at_ms = now_ms;
        item.last_reason = None;
    }

    fn acknowledge_receipt(
        state: &mut RuntimeState,
        receipt: &ReceiptCheck,
        acknowledged_at_ms: i64,
        acknowledged_by_hash: Option<String>,
        now_ms: i64,
    ) {
        let item = state
            .outbox
            .get_mut(&receipt.outbox_id)
            .expect("a validated receipt outbox item must exist");
        item.provider_acknowledged_at_ms = Some(acknowledged_at_ms);
        item.provider_acknowledged_by_hash = acknowledged_by_hash;
        item.next_receipt_check_at_ms = None;
        item.updated_at_ms = now_ms;
        item.last_reason = None;
        Self::push_receipt_history(state, receipt, "acknowledged", Some("pushover"), now_ms);
    }

    fn retry_receipt(
        state: &mut RuntimeState,
        receipt: &ReceiptCheck,
        reason: String,
        retry_after_ms: Option<u64>,
        now_ms: i64,
    ) {
        if now_ms >= receipt.receipt_expires_at_ms {
            Self::expire_receipt(
                state,
                receipt,
                receipt.receipt_expires_at_ms,
                now_ms,
                "provider_unacknowledged",
            );
            return;
        }
        let retry_interval_ms = retry_after_ms
            .unwrap_or(PUSHOVER_MIN_RETRY_INTERVAL_MS)
            .clamp(
                PUSHOVER_MIN_RETRY_INTERVAL_MS,
                receipt
                    .max_retry_interval_ms
                    .max(PUSHOVER_MIN_RETRY_INTERVAL_MS),
            );
        let item = state
            .outbox
            .get_mut(&receipt.outbox_id)
            .expect("a validated receipt outbox item must exist");
        item.next_receipt_check_at_ms = Some(add_millis(now_ms, retry_interval_ms));
        item.updated_at_ms = now_ms;
        item.last_reason = Some(redact_reason(reason));
    }

    fn fail_receipt(state: &mut RuntimeState, receipt: &ReceiptCheck, reason: String, now_ms: i64) {
        let reason = redact_reason(reason);
        let item = state
            .outbox
            .get_mut(&receipt.outbox_id)
            .expect("a validated receipt outbox item must exist");
        item.next_receipt_check_at_ms = None;
        item.updated_at_ms = now_ms;
        item.last_reason = Some(reason.clone());
        Self::push_receipt_history(state, receipt, "failed", Some(&reason), now_ms);
    }

    fn expire_receipt(
        state: &mut RuntimeState,
        receipt: &ReceiptCheck,
        expired_at_ms: i64,
        now_ms: i64,
        reason: &str,
    ) {
        let item = state
            .outbox
            .get_mut(&receipt.outbox_id)
            .expect("a validated receipt outbox item must exist");
        item.provider_expired_at_ms = Some(expired_at_ms);
        item.next_receipt_check_at_ms = None;
        item.updated_at_ms = now_ms;
        item.last_reason = Some(reason.to_owned());
        Self::push_receipt_history(state, receipt, "expired", Some(reason), now_ms);
    }

    fn push_receipt_history(
        state: &mut RuntimeState,
        receipt: &ReceiptCheck,
        outcome: &str,
        reason: Option<&str>,
        now_ms: i64,
    ) {
        let logical = state
            .logical
            .get(&receipt.logical_id)
            .expect("a validated receipt must reference a logical notification");
        let revision = logical.highest_revision;
        state.push_history(PendingHistoryEntry {
            logical_id: &receipt.logical_id,
            rule_id: &receipt.rule_id,
            revision,
            stage: receipt.stage,
            outcome,
            reason,
            occurred_at_ms: now_ms,
            next_eligible_at_ms: None,
        });
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
    let mut payload: ProviderPayload =
        serde_json::from_str(&delivery.payload_json).map_err(|_| "payload_invalid")?;
    let attachment = provider_attachment(delivery)?;
    if payload.canonical_attachment.is_some() && attachment.is_none() {
        payload.image_availability = Some("unavailable".to_owned());
    }
    if let Some(bytes) = attachment {
        payload.attachment_content_type = Some("image/jpeg".to_owned());
        payload.attachment_base64 = Some(STANDARD.encode(bytes));
    }
    serde_json::to_vec(&payload).map_err(|_| "payload_invalid")
}

fn provider_attachment(delivery: &Delivery) -> Result<Option<Vec<u8>>, &'static str> {
    if !delivery.attachment_enabled {
        return Ok(None);
    }
    let attachment = delivery
        .attachment_path
        .as_deref()
        .and_then(|path| read_bounded_attachment(path, delivery.max_attachment_bytes));
    if attachment.is_none() && delivery.attachment_required {
        return Err("attachment_unavailable");
    }
    Ok(attachment)
}

fn classify_http_failure(status: u16, headers: &ureq::http::HeaderMap) -> DeliveryOutcome {
    if status == 408 || status == 425 || status == 429 || status >= 500 {
        transient(
            "provider_retryable_status",
            Some(status),
            Some(
                retry_after_ms(headers)
                    .unwrap_or(PUSHOVER_MIN_RETRY_INTERVAL_MS)
                    .max(PUSHOVER_MIN_RETRY_INTERVAL_MS),
            ),
        )
    } else {
        permanent("provider_permanent_status", Some(status))
    }
}

fn classify_receipt_http_failure(status: u16, headers: &ureq::http::HeaderMap) -> ReceiptOutcome {
    if status == 408 || status == 425 || status == 429 || status >= 500 {
        receipt_transient(
            "provider_retryable_status",
            Some(
                retry_after_ms(headers)
                    .unwrap_or(PUSHOVER_MIN_RETRY_INTERVAL_MS)
                    .max(PUSHOVER_MIN_RETRY_INTERVAL_MS),
            ),
        )
    } else {
        receipt_permanent("provider_permanent_status")
    }
}

fn receipt_transient(reason: &str, retry_after_ms: Option<u64>) -> ReceiptOutcome {
    ReceiptOutcome::Transient {
        reason: reason.to_owned(),
        retry_after_ms,
    }
}

fn receipt_permanent(reason: &str) -> ReceiptOutcome {
    ReceiptOutcome::Permanent {
        reason: reason.to_owned(),
    }
}

fn transient(
    reason: &str,
    provider_status: Option<u16>,
    retry_after_ms: Option<u64>,
) -> DeliveryOutcome {
    DeliveryOutcome::Transient {
        reason: reason.to_owned(),
        provider_status,
        retry_after_ms,
    }
}

fn permanent(reason: &str, provider_status: Option<u16>) -> DeliveryOutcome {
    DeliveryOutcome::Permanent {
        reason: reason.to_owned(),
        provider_status,
    }
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_receipt(value: &str) -> bool {
    value.len() == 30 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn pushover_receipt_url(base_url: &str, receipt: &str, token: &str) -> anyhow::Result<url::Url> {
    if !valid_receipt(receipt) {
        anyhow::bail!("Pushover receipt is invalid");
    }
    let mut url = url::Url::parse(&format!("{base_url}{receipt}.json"))?;
    url.query_pairs_mut().append_pair("token", token);
    Ok(url)
}

fn seconds_to_millis(seconds: i64) -> Option<i64> {
    (seconds > 0).then(|| seconds.saturating_mul(1_000))
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

fn add_millis(timestamp_ms: i64, duration_ms: u64) -> i64 {
    timestamp_ms.saturating_add(i64::try_from(duration_ms).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::{io::Write as _, net::TcpListener, path::PathBuf, sync::mpsc};

    use super::*;
    use crate::notifications::{
        Lifecycle,
        model::{
            Action, AttachmentPolicy, Candidate, EnrichmentPolicy, FailurePolicy, Filter, Rule,
            Schedule, Severity, Template, Trigger,
        },
        state::{InboxReceipt, LogicalNotification, OutboxItem},
    };

    struct CapturedRequest {
        request_line: String,
        body: String,
    }

    fn provider_fixture(
        response_status: &str,
        response_body: &str,
    ) -> (
        String,
        mpsc::Receiver<CapturedRequest>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let response_status = response_status.to_owned();
        let response_body = response_body.to_owned();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4_096];
            let (header_end, content_length) = loop {
                let read = std::io::Read::read(&mut stream, &mut buffer).unwrap();
                assert_ne!(read, 0, "provider request ended before the headers");
                request.extend_from_slice(&buffer[..read]);
                if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or_default();
                    break (header_end + 4, content_length);
                }
            };
            while request.len() < header_end + content_length {
                let read = std::io::Read::read(&mut stream, &mut buffer).unwrap();
                assert_ne!(read, 0, "provider request ended before the body");
                request.extend_from_slice(&buffer[..read]);
            }
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let request_line = headers.lines().next().unwrap().to_owned();
            let body = String::from_utf8(request[header_end..header_end + content_length].to_vec())
                .unwrap();
            request_tx
                .send(CapturedRequest { request_line, body })
                .unwrap();
            write!(
                stream,
                "HTTP/1.1 {response_status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            )
            .unwrap();
        });
        (
            format!("http://{address}/1/messages.json"),
            request_rx,
            thread,
        )
    }

    fn timeout_fixture() -> (String, mpsc::SyncSender<()>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
        });
        (
            format!("http://{address}/1/messages.json"),
            release_tx,
            thread,
        )
    }

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
        let mut state = store.lock_state();
        state.logical.insert(
            logical_id.to_owned(),
            LogicalNotification {
                id: logical_id.to_owned(),
                rule_id: "rule-1".to_owned(),
                owner_id: "owner-1".to_owned(),
                source_id: "front-door".to_owned(),
                source_identity: "event-1".to_owned(),
                lifecycle: Lifecycle::Event,
                stage: Stage::Preliminary,
                highest_revision: 1,
                enrichment_attempts: 0,
                created_at_ms: 1_000,
                updated_at_ms: 1_000,
                enrichment_deadline_at_ms: 11_000,
                title: "Person".to_owned(),
                body: "Detected".to_owned(),
                deep_link: "/events/event-1".to_owned(),
                attachment_path: None,
                severity: Severity::Info,
                canonical_attachment: None,
                icon_key: None,
                image_available: false,
            },
        );
        state.receipts.insert(
            (logical_id.to_owned(), "owner-1".to_owned()),
            InboxReceipt::default(),
        );
        let id = state.next_outbox_id();
        state.outbox.insert(
            id,
            OutboxItem {
                id,
                logical_id: logical_id.to_owned(),
                action_index: 0,
                stage: Stage::Preliminary,
                channel,
                destination_json: r#"{"value":"https://secret.example/target"}"#.to_owned(),
                payload_json: r#"{"title":"Person"}"#.to_owned(),
                replacement_key: logical_id.to_owned(),
                priority: 0,
                status: OutboxStatus::Pending,
                attempt_count: 0,
                max_attempts,
                max_retry_interval_ms,
                attachment_enabled: false,
                attachment_required: false,
                max_attachment_bytes: 4_194_304,
                next_attempt_at_ms: 1_000,
                expires_at_ms,
                updated_at_ms: 1_000,
                last_reason: None,
                provider_request_id: None,
                provider_receipt: None,
                next_receipt_check_at_ms: None,
                provider_receipt_expires_at_ms: None,
                provider_acknowledged_at_ms: None,
                provider_expired_at_ms: None,
                provider_acknowledged_by_hash: None,
            },
        );
        drop(state);
        store
            .metrics
            .pending_deliveries
            .fetch_add(1, Ordering::Relaxed);
    }

    fn outbox(store: &Store, logical_id: &str) -> OutboxItem {
        store
            .lock_state()
            .outbox
            .values()
            .find(|item| item.logical_id == logical_id)
            .cloned()
            .unwrap()
    }

    fn attempt(store: &Store, logical_id: &str) -> DeliveryAttempt {
        store
            .lock_state()
            .attempts
            .iter()
            .find(|attempt| attempt.logical_id == logical_id)
            .cloned()
            .unwrap()
    }

    fn history_count(store: &Store, outcome: &str, reason: Option<&str>) -> usize {
        store
            .lock_state()
            .history
            .iter()
            .filter(|entry| {
                entry.outcome == outcome
                    && reason.is_none_or(|reason| entry.reason.as_deref() == Some(reason))
            })
            .count()
    }

    fn pushover_delivery() -> Delivery {
        let destination = pushover::Destination {
            application_token: "a23456789012345678901234567890".to_owned(),
            user_key: "u23456789012345678901234567890".to_owned(),
            device: None,
            sound: None,
            priority: 0,
            retry_seconds: None,
            expire_seconds: None,
            deep_link_base_url: Some("https://keeppeek.example/".to_owned()),
        };
        Delivery {
            id: 1,
            logical_id: "logical-1".to_owned(),
            rule_id: "rule-1".to_owned(),
            stage: Stage::Preliminary,
            channel: Channel::Push,
            destination_json: serde_json::to_string(&Destination {
                value: serde_json::to_string(&destination).unwrap(),
            })
            .unwrap(),
            payload_json: serde_json::json!({
                "title": "Motion at Front Door",
                "body": "Motion detected",
                "deep_link": "/events/event-1",
                "occurred_at_ms": 1_725_000_123_456_i64
            })
            .to_string(),
            replacement_key: "logical-1".to_owned(),
            attempt: 1,
            max_attempts: 3,
            max_retry_interval_ms: 30_000,
            attachment_path: None,
            attachment_enabled: false,
            attachment_required: false,
            max_attachment_bytes: 1_048_576,
            expires_at_ms: i64::MAX,
        }
    }

    #[test]
    fn successful_delivery_is_recorded_and_target_is_hashed() {
        let directory = test_dir("notification-delivery-success");
        let store = Store::open(&directory.join("config.toml")).unwrap();
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

        assert_eq!(outbox(&store, "logical-1").status, OutboxStatus::Delivered);
        assert_eq!(history_count(&store, "delivered", None), 1);
        let target_hash = attempt(&store, "logical-1").target_hash;
        assert_eq!(target_hash.len(), 64);
        assert!(!target_hash.contains("secret.example"));
        let metrics = store.metrics.snapshot();
        assert_eq!(metrics.pending_deliveries, 0);
        assert_eq!(metrics.delivery_attempts, 1);
        assert_eq!(metrics.delivery_successes, 1);
        assert_eq!(metrics.delivery_failures, 0);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn duplicate_delivery_completion_does_not_drift_state_or_metrics() {
        let directory = test_dir("notification-delivery-duplicate-finish");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        seed_outbox(&store, "logical-1", Channel::Browser, 3, 5_000, 10_000);
        let delivery = store.claim_due(Channel::Browser, 1_000).unwrap().unwrap();
        let outcome = DeliveryOutcome::Delivered {
            provider_status: None,
        };

        store
            .finish_delivery(&delivery, outcome.clone(), 1_100)
            .unwrap();
        let metrics = store.metrics.snapshot();
        let attempt_count = store.lock_state().attempts.len();
        let delivered_history_count = history_count(&store, "delivered", None);

        assert!(
            store
                .finish_delivery(&delivery, outcome, 1_200)
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
        assert_eq!(store.metrics.snapshot(), metrics);
        assert_eq!(store.lock_state().attempts.len(), attempt_count);
        assert_eq!(
            history_count(&store, "delivered", None),
            delivered_history_count
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn transient_delivery_respects_retry_after_and_max_attempts() {
        let directory = test_dir("notification-delivery-retry");
        let store = Store::open(&directory.join("config.toml")).unwrap();
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
        assert_eq!(outbox(&store, "logical-1").status, OutboxStatus::Failed);
        assert_eq!(store.lock_state().attempts.len(), 2);
        let metrics = store.metrics.snapshot();
        assert_eq!(metrics.pending_deliveries, 0);
        assert_eq!(metrics.delivery_attempts, 2);
        assert_eq!(metrics.delivery_retries, 1);
        assert_eq!(metrics.delivery_failures, 1);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn expired_and_unrelated_channel_items_do_not_block_claiming() {
        let directory = test_dir("notification-delivery-expiry");
        let store = Store::open(&directory.join("config.toml")).unwrap();
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
            outbox(&store, "logical-webhook").status,
            OutboxStatus::Expired
        );
        let browser = store.claim_due(Channel::Browser, 1_500).unwrap().unwrap();
        assert_eq!(browser.logical_id, "logical-browser");
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn permanent_provider_failure_is_not_retried() {
        let directory = test_dir("notification-delivery-permanent");
        let store = Store::open(&directory.join("config.toml")).unwrap();
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
        assert_eq!(outbox(&store, "logical-1").status, OutboxStatus::Failed);
        assert!(store.claim_due(Channel::Push, 10_000).unwrap().is_none());
        assert_eq!(
            attempt(&store, "logical-1").reason.as_deref(),
            Some("channel_unavailable")
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
            payload_json: serde_json::json!({
                "title": "Person",
                "body": "Detected",
                "deep_link": "/events",
                "event_id": "event-1",
                "event_revision": 3,
                "canonical_attachment": {
                    "id": "snapshot-hero",
                    "attachment_type": "snapshot",
                    "content_type": "image/jpeg",
                    "byte_len": 4,
                    "ordinal": 0,
                    "timestamp_ms": 1_725_000_123_456_i64,
                    "text": null
                },
                "icon_key": "person",
                "image_availability": "available"
            })
            .to_string(),
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
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["event_id"], "event-1");
        assert_eq!(payload["event_revision"], 3);
        assert_eq!(payload["canonical_attachment"]["id"], "snapshot-hero");
        assert_eq!(payload["icon_key"], "person");
        assert_eq!(payload["image_availability"], "available");

        std::fs::write(&image_path, [1_u8, 2, 3, 4, 5]).unwrap();
        assert_eq!(webhook_payload(&delivery), Err("attachment_unavailable"));
        delivery.attachment_required = false;
        let fallback = String::from_utf8(webhook_payload(&delivery).unwrap()).unwrap();
        assert!(!fallback.contains("attachment_base64"));
        let fallback: serde_json::Value = serde_json::from_str(&fallback).unwrap();
        assert_eq!(fallback["canonical_attachment"]["id"], "snapshot-hero");
        assert_eq!(fallback["image_availability"], "unavailable");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pushover_provider_sends_authenticated_normal_message_with_event_fields() {
        let directory = test_dir("notification-pushover-provider");
        let image_path = directory.join("event-1.jpg");
        std::fs::write(&image_path, [1_u8, 2, 3, 4]).unwrap();
        let destination = serde_json::to_string(&pushover::Destination {
            application_token: "a23456789012345678901234567890".to_owned(),
            user_key: "u23456789012345678901234567890".to_owned(),
            device: Some("front-door-phone".to_owned()),
            sound: Some("pushover".to_owned()),
            priority: 0,
            retry_seconds: None,
            expire_seconds: None,
            deep_link_base_url: Some("https://keeppeek.example/".to_owned()),
        })
        .unwrap();
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let rule = Rule {
            id: "front-door-motion".to_owned(),
            name: "Front Door Motion".to_owned(),
            enabled: true,
            revision: 0,
            owner_id: "owner-1".to_owned(),
            triggers: vec![Trigger::EventCreated],
            filter: Filter {
                event_kinds: vec!["motion".to_owned()],
                ..Filter::default()
            },
            schedule: Schedule {
                timezone: "UTC".to_owned(),
                active_windows: Vec::new(),
                quiet_hours: None,
            },
            cooldowns: Vec::new(),
            rate_limits: Vec::new(),
            critical_bypass: None,
            enrichment: EnrichmentPolicy {
                deadline_ms: 10_000,
                maximum_revisions: 4,
                maximum_attempts: 2,
                maximum_attachment_bytes: 4,
                wake_after_deadline: false,
            },
            actions: vec![Action {
                enabled: true,
                channel: Channel::Push,
                destination,
                template: Template {
                    title: "{{event.kind}} at {{source.name}}".to_owned(),
                    body: "{{event.kind}} detected by {{source.id}}".to_owned(),
                },
                attachment: AttachmentPolicy::Required,
                allow_second_delivery: false,
            }],
            failure: FailurePolicy {
                maximum_attempts: 3,
                maximum_retry_interval_ms: 30_000,
                expiry_ms: 3_600_000,
            },
        };
        let saved = store.save_draft(rule, 0, 100).unwrap();
        store
            .activate(
                &saved.id,
                &saved.owner_id,
                saved.active_revision,
                saved.draft_revision,
                200,
            )
            .unwrap();
        let occurred_at_ms = 1_725_000_123_456_i64;
        let summary = store
            .process(Candidate {
                trigger: Trigger::EventCreated,
                source_id: "front-door".to_owned(),
                source_name: Some("Front Door".to_owned()),
                source_identity: "event-1".to_owned(),
                lifecycle: Lifecycle::Event,
                event_kind: Some("motion".to_owned()),
                payload: None,
                group_ids: Vec::new(),
                zone: None,
                confidence: Some(0.9),
                attachment_path: Some(image_path.to_string_lossy().into_owned()),
                canonical_attachment: None,
                icon_key: Some("motion".to_owned()),
                image_available: true,
                duration_ms: None,
                severity: Severity::Info,
                reviewed: Some(false),
                bookmarked: Some(false),
                privacy_active: false,
                revision: 1,
                stage: Stage::Preliminary,
                occurred_at_ms,
                deep_link: "/events?camera=front-door&event=event-1".to_owned(),
            })
            .unwrap();
        assert_eq!(summary.queued_attempts, 1);
        let delivery = store
            .claim_due(Channel::Push, occurred_at_ms)
            .unwrap()
            .unwrap();
        let (messages_url, request_rx, fixture_thread) = provider_fixture(
            "200 OK",
            r#"{"status":1,"request":"647d2300-702c-4b38-8b2f-d56326ae460b"}"#,
        );

        let result = PushoverProvider::with_messages_url(messages_url).deliver(&delivery);
        assert_eq!(
            result.outcome,
            DeliveryOutcome::Delivered {
                provider_status: Some(200)
            }
        );
        assert_eq!(
            result.request_id.as_deref(),
            Some("647d2300-702c-4b38-8b2f-d56326ae460b")
        );
        assert!(result.receipt.is_none());
        let request = request_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        fixture_thread.join().unwrap();
        assert_eq!(request.request_line, "POST /1/messages.json HTTP/1.1");
        let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["token"], "a23456789012345678901234567890");
        assert_eq!(body["user"], "u23456789012345678901234567890");
        assert_eq!(body["device"], "front-door-phone");
        assert_eq!(body["sound"], "pushover");
        assert_eq!(body["priority"], 0);
        assert_eq!(body["title"], "motion at Front Door");
        assert_eq!(body["message"], "motion detected by front-door");
        assert_eq!(body["timestamp"], 1_725_000_123_i64);
        assert_eq!(
            body["url"],
            "https://keeppeek.example/events?camera=front-door&event=event-1"
        );
        assert_eq!(body["url_title"], "Open in KeepPeek");
        assert_eq!(body["attachment_base64"], "AQIDBA==");
        assert_eq!(body["attachment_type"], "image/jpeg");
        assert!(body.get("retry").is_none());
        assert!(body.get("expire").is_none());
        assert!(!request.body.contains(image_path.to_string_lossy().as_ref()));
        store
            .finish_delivery(&delivery, result, occurred_at_ms.saturating_add(1))
            .unwrap();
        assert_eq!(
            attempt(&store, &delivery.logical_id)
                .provider_request_id
                .as_deref(),
            Some("647d2300-702c-4b38-8b2f-d56326ae460b")
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pushover_provider_classifies_rate_limits_transient_failures_and_invalid_credentials() {
        for (status, expected) in [
            (
                "429 Too Many Requests",
                DeliveryOutcome::Transient {
                    reason: "provider_retryable_status".to_owned(),
                    provider_status: Some(429),
                    retry_after_ms: Some(PUSHOVER_MIN_RETRY_INTERVAL_MS),
                },
            ),
            (
                "503 Service Unavailable",
                DeliveryOutcome::Transient {
                    reason: "provider_retryable_status".to_owned(),
                    provider_status: Some(503),
                    retry_after_ms: Some(PUSHOVER_MIN_RETRY_INTERVAL_MS),
                },
            ),
            (
                "400 Bad Request",
                DeliveryOutcome::Permanent {
                    reason: "provider_permanent_status".to_owned(),
                    provider_status: Some(400),
                },
            ),
        ] {
            let (messages_url, request_rx, fixture_thread) =
                provider_fixture(status, r#"{"status":0,"errors":["invalid"]}"#);
            let result =
                PushoverProvider::with_messages_url(messages_url).deliver(&pushover_delivery());
            request_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            fixture_thread.join().unwrap();
            assert_eq!(result.outcome, expected);
            assert!(result.request_id.is_none());
        }
    }

    #[test]
    fn pushover_provider_timeout_is_retryable_with_a_bounded_delay() {
        let (messages_url, release_tx, fixture_thread) = timeout_fixture();
        let provider = PushoverProvider::with_urls_and_timeout(
            messages_url,
            PUSHOVER_RECEIPTS_URL,
            Duration::from_millis(20),
        );

        let result = provider.deliver(&pushover_delivery());
        let _ = release_tx.send(());
        fixture_thread.join().unwrap();
        assert_eq!(
            result.outcome,
            DeliveryOutcome::Transient {
                reason: "provider_timeout".to_owned(),
                provider_status: None,
                retry_after_ms: Some(PUSHOVER_MIN_RETRY_INTERVAL_MS),
            }
        );
    }

    #[test]
    fn pushover_provider_reads_emergency_acknowledgement_without_exposing_identity() {
        let acknowledged_by = "u23456789012345678901234567890";
        let response = format!(
            "{{\"status\":1,\"request\":\"647d2300-702c-4b38-8b2f-d56326ae460b\",\"acknowledged\":1,\"acknowledged_at\":1725000123,\"acknowledged_by\":\"{acknowledged_by}\",\"expired\":0,\"expires_at\":1725000423}}"
        );
        let (fixture_url, request_rx, fixture_thread) = provider_fixture("200 OK", &response);
        let origin = fixture_url.strip_suffix("/1/messages.json").unwrap();
        let destination = pushover::Destination {
            application_token: "a23456789012345678901234567890".to_owned(),
            user_key: acknowledged_by.to_owned(),
            device: None,
            sound: None,
            priority: 2,
            retry_seconds: Some(30),
            expire_seconds: Some(300),
            deep_link_base_url: None,
        };
        let check = ReceiptCheck {
            outbox_id: 1,
            logical_id: "logical-1".to_owned(),
            rule_id: "rule-1".to_owned(),
            stage: Stage::Preliminary,
            destination_json: serde_json::to_string(&Destination {
                value: serde_json::to_string(&destination).unwrap(),
            })
            .unwrap(),
            receipt: "r23456789012345678901234567890".to_owned(),
            receipt_expires_at_ms: 1_725_000_423_000,
            max_retry_interval_ms: 30_000,
        };
        let provider = PushoverProvider::with_urls(
            format!("{origin}/1/messages.json"),
            format!("{origin}/1/receipts/"),
        );

        let outcome = provider.check_receipt(&check);
        let request = request_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        fixture_thread.join().unwrap();
        assert_eq!(
            request.request_line,
            "GET /1/receipts/r23456789012345678901234567890.json?token=a23456789012345678901234567890 HTTP/1.1"
        );
        assert_eq!(
            outcome,
            ReceiptOutcome::Acknowledged {
                acknowledged_at_ms: 1_725_000_123_000,
                acknowledged_by_hash: Some(target_hash(acknowledged_by)),
            }
        );
        assert!(!format!("{outcome:?}").contains(acknowledged_by));
    }

    #[test]
    fn emergency_receipt_is_private_and_acknowledgement_is_recorded() {
        let directory = test_dir("notification-pushover-receipt");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        seed_outbox(&store, "logical-1", Channel::Push, 3, 30_000, 600_000);
        let delivery = store.claim_due(Channel::Push, 1_000).unwrap().unwrap();
        store
            .finish_delivery(
                &delivery,
                ProviderResult {
                    outcome: DeliveryOutcome::Delivered {
                        provider_status: Some(200),
                    },
                    request_id: Some("647d2300-702c-4b38-8b2f-d56326ae460b".to_owned()),
                    receipt: Some(ProviderReceipt {
                        id: "r23456789012345678901234567890".to_owned(),
                        expire_seconds: 300,
                    }),
                },
                1_100,
            )
            .unwrap();

        assert!(store.claim_due_receipt(6_099).unwrap().is_none());
        let receipt = store.claim_due_receipt(6_100).unwrap().unwrap();
        assert_eq!(receipt.receipt, "r23456789012345678901234567890");
        store
            .finish_receipt(
                &receipt,
                ReceiptOutcome::Acknowledged {
                    acknowledged_at_ms: 6_000,
                    acknowledged_by_hash: Some(target_hash("user-key")),
                },
                6_200,
            )
            .unwrap();

        assert_eq!(
            attempt(&store, "logical-1").provider_request_id.as_deref(),
            Some("647d2300-702c-4b38-8b2f-d56326ae460b")
        );
        assert_eq!(
            outbox(&store, "logical-1")
                .provider_acknowledged_by_hash
                .as_deref(),
            Some(target_hash("user-key").as_str())
        );
        assert_eq!(history_count(&store, "acknowledged", None), 1);
        let history = store.history("owner-1", 10).unwrap();
        let attempt = &history[0].attempts[0];
        assert_eq!(
            attempt.provider_request_id.as_deref(),
            Some("647d2300-702c-4b38-8b2f-d56326ae460b")
        );
        assert_eq!(
            attempt.provider_acknowledgement_state.as_deref(),
            Some("acknowledged")
        );
        assert_eq!(attempt.provider_acknowledged_at_ms, Some(6_000));
        assert_eq!(
            attempt.provider_acknowledged_by_hash.as_deref(),
            Some(target_hash("user-key").as_str())
        );
        assert!(store.claim_due_receipt(100_000).unwrap().is_none());
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unacknowledged_emergency_receipt_expires_once_and_stops_polling() {
        let directory = test_dir("notification-pushover-receipt-expiry");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        seed_outbox(&store, "logical-1", Channel::Push, 3, 30_000, 600_000);
        let delivery = store.claim_due(Channel::Push, 1_000).unwrap().unwrap();
        store
            .finish_delivery(
                &delivery,
                ProviderResult {
                    outcome: DeliveryOutcome::Delivered {
                        provider_status: Some(200),
                    },
                    request_id: Some("647d2300-702c-4b38-8b2f-d56326ae460b".to_owned()),
                    receipt: Some(ProviderReceipt {
                        id: "r23456789012345678901234567890".to_owned(),
                        expire_seconds: 30,
                    }),
                },
                1_100,
            )
            .unwrap();

        let receipt = store.claim_due_receipt(6_100).unwrap().unwrap();
        store
            .finish_receipt(
                &receipt,
                ReceiptOutcome::Expired {
                    expired_at_ms: Some(31_100),
                },
                31_200,
            )
            .unwrap();

        assert_eq!(outbox(&store, "logical-1").status, OutboxStatus::Delivered);
        assert_eq!(
            history_count(&store, "expired", Some("provider_unacknowledged")),
            1
        );
        assert!(store.claim_due_receipt(100_000).unwrap().is_none());
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
