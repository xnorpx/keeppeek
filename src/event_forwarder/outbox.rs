use super::model::Publication;
use std::{
    collections::{HashSet, VecDeque},
    fmt,
    sync::{Arc, Mutex},
};

const MAX_PENDING_ITEMS: usize = 10_000;
const MAX_DELIVERY_RECEIPTS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OutboxItem {
    pub sequence: i64,
    pub publication: Publication,
    pub attempts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OutboxStats {
    pub pending_items: u64,
    pub pending_bytes: u64,
    pub oldest_event_timestamp_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnqueueResult {
    Inserted,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OutboxFull {
    pub limit_bytes: u64,
    pub attempted_bytes: u64,
}

impl fmt::Display for OutboxFull {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "MQTT outbox capacity exceeded: {} bytes would exceed the {} byte limit",
            self.attempted_bytes, self.limit_bytes
        )
    }
}

impl std::error::Error for OutboxFull {}

#[derive(Default)]
struct OutboxState {
    next_sequence: i64,
    pending: VecDeque<(OutboxItem, u64)>,
    pending_bytes: u64,
    seen_keys: HashSet<String>,
    delivered_keys: VecDeque<String>,
}

#[derive(Clone, Default)]
pub(super) struct Outbox {
    state: Arc<Mutex<OutboxState>>,
}

impl Outbox {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn enqueue(
        &self,
        publication: &Publication,
        limit_bytes: u64,
        _now_ms: i64,
    ) -> anyhow::Result<EnqueueResult> {
        let mut state = self.state.lock().expect("MQTT outbox state lock poisoned");
        if state.seen_keys.contains(&publication.dedup_key) {
            return Ok(EnqueueResult::Duplicate);
        }
        if state.pending.len() == MAX_PENDING_ITEMS {
            anyhow::bail!("MQTT outbox capacity exceeded: item limit reached");
        }
        let item_bytes = publication_size(publication)?;
        let attempted_bytes = state.pending_bytes.saturating_add(item_bytes);
        if attempted_bytes > limit_bytes {
            return Err(OutboxFull {
                limit_bytes,
                attempted_bytes,
            }
            .into());
        }
        state.next_sequence = state
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("MQTT outbox sequence exhausted"))?;
        let sequence = state.next_sequence;
        state.seen_keys.insert(publication.dedup_key.clone());
        state.pending_bytes = attempted_bytes;
        state.pending.push_back((
            OutboxItem {
                sequence,
                publication: publication.clone(),
                attempts: 0,
            },
            item_bytes,
        ));
        Ok(EnqueueResult::Inserted)
    }

    pub(super) fn next(&self) -> anyhow::Result<Option<OutboxItem>> {
        let state = self.state.lock().expect("MQTT outbox state lock poisoned");
        Ok(state.pending.front().map(|(item, _)| item.clone()))
    }

    pub(super) fn mark_attempt(&self, sequence: i64, error: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock().expect("MQTT outbox state lock poisoned");
        let item = state
            .pending
            .iter_mut()
            .find(|(item, _)| item.sequence == sequence)
            .map(|(item, _)| item)
            .ok_or_else(|| anyhow::anyhow!("MQTT outbox item was not found"))?;
        item.attempts = item.attempts.saturating_add(1);
        tracing::warn!(
            event = "mqtt_delivery_retry",
            dedup_key = %item.publication.dedup_key,
            attempts = item.attempts,
            reason = %error,
        );
        Ok(())
    }

    pub(super) fn mark_delivered(&self, sequence: i64, delivered_at_ms: i64) -> anyhow::Result<()> {
        let mut state = self.state.lock().expect("MQTT outbox state lock poisoned");
        let index = state
            .pending
            .iter()
            .position(|(item, _)| item.sequence == sequence)
            .ok_or_else(|| anyhow::anyhow!("MQTT outbox item was not found"))?;
        let (item, item_bytes) = state
            .pending
            .remove(index)
            .expect("MQTT outbox index must remain valid while locked");
        state.pending_bytes = state.pending_bytes.saturating_sub(item_bytes);
        state
            .delivered_keys
            .push_back(item.publication.dedup_key.clone());
        if state.delivered_keys.len() > MAX_DELIVERY_RECEIPTS
            && let Some(expired) = state.delivered_keys.pop_front()
        {
            state.seen_keys.remove(&expired);
        }
        tracing::info!(
            event = "mqtt_delivery_succeeded",
            dedup_key = %item.publication.dedup_key,
            delivered_at_ms,
            attempts = item.attempts,
        );
        Ok(())
    }

    pub(super) fn stats(&self) -> anyhow::Result<OutboxStats> {
        let state = self.state.lock().expect("MQTT outbox state lock poisoned");
        Ok(OutboxStats {
            pending_items: u64::try_from(state.pending.len())?,
            pending_bytes: state.pending_bytes,
            oldest_event_timestamp_ms: state
                .pending
                .iter()
                .map(|(item, _)| item.publication.event_timestamp_ms)
                .min(),
        })
    }
}

fn publication_size(publication: &Publication) -> anyhow::Result<u64> {
    u64::try_from(
        publication
            .dedup_key
            .len()
            .saturating_add(publication.topic.len())
            .saturating_add(publication.payload.len())
            .saturating_add(publication.content_type.len())
            .saturating_add(publication.correlation_data.len()),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication(revision: u64) -> Publication {
        Publication {
            dedup_key: format!("event:home-nvr:motion-42:{revision}"),
            topic: "keeppeek/home-nvr/sources/front-door/events/motion".to_owned(),
            payload: format!(r#"{{"event_id":"motion-42","revision":{revision}}}"#).into_bytes(),
            qos: 1,
            retain: false,
            event_timestamp_ms: 1_786_800_000_000,
            content_type: "application/json".to_owned(),
            payload_format_indicator: Some(1),
            correlation_data: b"motion-42".to_vec(),
        }
    }

    #[test]
    fn deduplicates_event_revisions_until_process_state_resets() {
        let outbox = Outbox::new();
        assert_eq!(
            outbox
                .enqueue(&publication(1), 1_024 * 1_024, 1_786_800_000_001)
                .unwrap(),
            EnqueueResult::Inserted
        );
        assert_eq!(
            outbox
                .enqueue(&publication(1), 1_024 * 1_024, 1_786_800_000_002)
                .unwrap(),
            EnqueueResult::Duplicate
        );
        assert_eq!(
            outbox
                .enqueue(&publication(2), 1_024 * 1_024, 1_786_800_000_003)
                .unwrap(),
            EnqueueResult::Inserted
        );
        let first = outbox.next().unwrap().unwrap();
        assert_eq!(first.publication.dedup_key, "event:home-nvr:motion-42:1");
        assert_eq!(outbox.stats().unwrap().pending_items, 2);

        let restarted = Outbox::new();
        assert!(restarted.next().unwrap().is_none());
        assert_eq!(
            restarted
                .enqueue(&publication(1), 1_024 * 1_024, 1_786_800_000_004)
                .unwrap(),
            EnqueueResult::Inserted
        );
    }

    #[test]
    fn rejects_insert_before_exceeding_memory_budget() {
        let outbox = Outbox::new();
        let error = outbox
            .enqueue(&publication(1), 1, 1_786_800_000_001)
            .unwrap_err();
        assert!(error.downcast_ref::<OutboxFull>().is_some());
        assert_eq!(outbox.stats().unwrap().pending_items, 0);
    }

    #[test]
    fn failed_delivery_keeps_identity_until_acknowledged() {
        let outbox = Outbox::new();
        outbox
            .enqueue(&publication(7), 1_024 * 1_024, 1_786_800_000_001)
            .unwrap();
        let item = outbox.next().unwrap().unwrap();
        outbox
            .mark_attempt(item.sequence, "broker unavailable")
            .unwrap();
        let retry = outbox.next().unwrap().unwrap();
        assert_eq!(retry.publication.dedup_key, item.publication.dedup_key);
        assert_eq!(retry.publication.payload, item.publication.payload);
        assert_eq!(retry.attempts, 1);
        outbox
            .mark_delivered(retry.sequence, 1_786_800_000_100)
            .unwrap();
        assert!(outbox.next().unwrap().is_none());
        assert_eq!(
            outbox
                .enqueue(&publication(7), 1_024 * 1_024, 1_786_800_000_200)
                .unwrap(),
            EnqueueResult::Duplicate
        );
    }
}
