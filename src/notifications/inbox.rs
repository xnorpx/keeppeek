use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use super::{
    RuleStoreError, Stage,
    model::Severity,
    state::{DeliveryAttempt, InboxReceipt, LogicalNotification, RuntimeState},
    store::Store,
};
use crate::storage::metadata::EventAttachment;

const MAX_PAGE_ITEMS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationItem {
    pub logical_id: String,
    pub rule_id: String,
    pub source_id: String,
    pub source_identity: String,
    pub lifecycle: String,
    pub stage: Stage,
    pub revision: u64,
    pub title: String,
    pub body: String,
    pub deep_link: String,
    pub attachment_available: bool,
    pub canonical_attachment: Option<EventAttachment>,
    pub icon_key: Option<String>,
    pub image_available: bool,
    pub severity: Severity,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub seen_at_ms: Option<i64>,
    pub acknowledged_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbox {
    pub items: Vec<NotificationItem>,
    pub unread_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEvent {
    pub sequence: u64,
    pub revision: u64,
    pub stage: Stage,
    pub outcome: String,
    pub reason: Option<String>,
    pub occurred_at_ms: i64,
    pub next_eligible_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRecord {
    pub sequence: u64,
    pub channel: String,
    pub stage: Stage,
    pub attempt: u32,
    pub outcome: String,
    pub target_hash: String,
    pub provider_status: Option<u16>,
    pub provider_request_id: Option<String>,
    pub provider_acknowledged_at_ms: Option<i64>,
    pub provider_expired_at_ms: Option<i64>,
    pub provider_acknowledged_by_hash: Option<String>,
    pub provider_acknowledgement_state: Option<String>,
    pub reason: Option<String>,
    pub attempted_at_ms: i64,
    pub retry_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryGroup {
    pub notification: NotificationItem,
    pub events: Vec<HistoryEvent>,
    pub attempts: Vec<AttemptRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClearScope {
    All,
    Rule(String),
    Before(i64),
}

#[derive(Clone)]
struct InboxRecord {
    logical: LogicalNotification,
    receipt: InboxReceipt,
}

#[derive(Clone, Copy)]
enum ReceiptUpdate {
    Seen,
    Acknowledged,
    Cleared,
}

impl Store {
    pub(super) fn inbox(&self, principal_id: &str, limit: usize) -> anyhow::Result<Inbox> {
        let (records, unread_count) = {
            let state = self.lock_state();
            let records = Self::notification_records(&state, principal_id, limit, false);
            let unread_count = state
                .receipts
                .iter()
                .filter(|((logical_id, receipt_principal), receipt)| {
                    receipt_principal == principal_id
                        && receipt.seen_at_ms.is_none()
                        && receipt.cleared_at_ms.is_none()
                        && state
                            .logical
                            .get(logical_id)
                            .is_some_and(|logical| logical.owner_id == principal_id)
                })
                .count();
            (records, unread_count)
        };
        Ok(Inbox {
            items: records.iter().map(Self::notification_item).collect(),
            unread_count: u64::try_from(unread_count)
                .expect("bounded notification count must fit in u64"),
        })
    }

    pub(super) fn history(
        &self,
        principal_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<HistoryGroup>> {
        let snapshots = {
            let state = self.lock_state();
            let records = Self::notification_records(&state, principal_id, limit, true);
            let logical_ids = records
                .iter()
                .map(|record| record.logical.id.clone())
                .collect::<HashSet<_>>();
            let mut events = HashMap::<String, Vec<HistoryEvent>>::new();
            for entry in state
                .history
                .iter()
                .filter(|entry| logical_ids.contains(&entry.logical_id))
            {
                let logical = state
                    .logical
                    .get(&entry.logical_id)
                    .expect("notification history must reference a logical notification");
                assert_eq!(
                    entry.rule_id, logical.rule_id,
                    "notification history must reference the logical notification rule"
                );
                events
                    .entry(entry.logical_id.clone())
                    .or_default()
                    .push(HistoryEvent {
                        sequence: entry.sequence,
                        revision: entry.revision,
                        stage: entry.stage,
                        outcome: entry.outcome.clone(),
                        reason: entry.reason.clone(),
                        occurred_at_ms: entry.occurred_at_ms,
                        next_eligible_at_ms: entry.next_eligible_at_ms,
                    });
            }
            let mut attempts = HashMap::<String, Vec<AttemptRecord>>::new();
            for attempt in state
                .attempts
                .iter()
                .filter(|attempt| logical_ids.contains(&attempt.logical_id))
            {
                attempts
                    .entry(attempt.logical_id.clone())
                    .or_default()
                    .push(Self::attempt_record(&state, attempt));
            }
            records
                .into_iter()
                .map(|record| {
                    let logical_id = record.logical.id.clone();
                    (
                        record,
                        events.remove(&logical_id).unwrap_or_default(),
                        attempts.remove(&logical_id).unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>()
        };
        Ok(snapshots
            .iter()
            .map(|(record, events, attempts)| HistoryGroup {
                notification: Self::notification_item(record),
                events: events.clone(),
                attempts: attempts.clone(),
            })
            .collect())
    }

    pub(super) fn mark_seen(
        &self,
        logical_id: &str,
        principal_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        self.update_receipt(
            logical_id,
            principal_id,
            ReceiptUpdate::Seen,
            now_ms,
            "notification_seen",
        )
    }

    pub(super) fn acknowledge(
        &self,
        logical_id: &str,
        principal_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        self.update_receipt(
            logical_id,
            principal_id,
            ReceiptUpdate::Acknowledged,
            now_ms,
            "notification_acknowledged",
        )
    }

    pub(super) fn clear(
        &self,
        logical_id: &str,
        principal_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        self.update_receipt(
            logical_id,
            principal_id,
            ReceiptUpdate::Cleared,
            now_ms,
            "notification_cleared",
        )
    }

    pub(super) fn clear_scope(
        &self,
        principal_id: &str,
        scope: &ClearScope,
        now_ms: i64,
    ) -> anyhow::Result<u64> {
        let detail = match scope {
            ClearScope::All => "all".to_owned(),
            ClearScope::Rule(rule_id) => format!("rule:{rule_id}"),
            ClearScope::Before(before_ms) => format!("before:{before_ms}"),
        };
        let changed = {
            let mut state = self.lock_state();
            let keys = state
                .receipts
                .iter()
                .filter(|((logical_id, receipt_principal), receipt)| {
                    receipt_principal == principal_id
                        && receipt.cleared_at_ms.is_none()
                        && Self::clear_scope_matches(&state, logical_id, principal_id, scope)
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for key in &keys {
                if let Some(receipt) = state.receipts.get_mut(key) {
                    receipt.cleared_at_ms = Some(now_ms);
                }
            }
            state.prune(now_ms)?;
            u64::try_from(keys.len()).expect("bounded notification count must fit in u64")
        };
        self.record_inbox_audit(
            principal_id,
            "notifications_cleared",
            "notifications",
            Some(&detail),
            now_ms,
        );
        Ok(changed)
    }

    fn notification_records(
        state: &RuntimeState,
        principal_id: &str,
        limit: usize,
        include_cleared: bool,
    ) -> Vec<InboxRecord> {
        let limit = limit.clamp(1, MAX_PAGE_ITEMS);
        let mut records = state
            .receipts
            .iter()
            .filter_map(|((logical_id, receipt_principal), receipt)| {
                if receipt_principal != principal_id
                    || (!include_cleared && receipt.cleared_at_ms.is_some())
                {
                    return None;
                }
                let logical = state.logical.get(logical_id)?;
                (logical.owner_id == principal_id).then(|| InboxRecord {
                    logical: logical.clone(),
                    receipt: receipt.clone(),
                })
            })
            .collect::<Vec<_>>();
        records.sort_unstable_by(|left, right| {
            right
                .logical
                .updated_at_ms
                .cmp(&left.logical.updated_at_ms)
                .then_with(|| left.logical.id.cmp(&right.logical.id))
        });
        records.truncate(limit);
        records
    }

    fn notification_item(record: &InboxRecord) -> NotificationItem {
        let logical = &record.logical;
        let attachment_available = logical
            .attachment_path
            .as_deref()
            .is_some_and(|path| Path::new(path).is_file());
        NotificationItem {
            logical_id: logical.id.clone(),
            rule_id: logical.rule_id.clone(),
            source_id: logical.source_id.clone(),
            source_identity: logical.source_identity.clone(),
            lifecycle: logical.lifecycle.as_str().to_owned(),
            stage: logical.stage,
            revision: logical.highest_revision,
            title: logical.title.clone(),
            body: logical.body.clone(),
            deep_link: logical.deep_link.clone(),
            attachment_available,
            canonical_attachment: logical.canonical_attachment.clone(),
            icon_key: logical.icon_key.clone(),
            image_available: logical.canonical_attachment.is_some()
                && logical.image_available
                && attachment_available,
            severity: logical.severity,
            created_at_ms: logical.created_at_ms,
            updated_at_ms: logical.updated_at_ms,
            seen_at_ms: record.receipt.seen_at_ms,
            acknowledged_at_ms: record.receipt.acknowledged_at_ms,
        }
    }

    fn attempt_record(state: &RuntimeState, attempt: &DeliveryAttempt) -> AttemptRecord {
        let delivered_outbox = (attempt.outcome == "delivered")
            .then(|| state.outbox.get(&attempt.outbox_id))
            .flatten();
        let provider_acknowledgement_state = delivered_outbox.and_then(|outbox| {
            outbox.provider_receipt.as_ref()?;
            Some(
                if outbox.provider_acknowledged_at_ms.is_some() {
                    "acknowledged"
                } else if outbox.provider_expired_at_ms.is_some() {
                    "expired"
                } else if outbox.next_receipt_check_at_ms.is_none() {
                    "failed"
                } else {
                    "pending"
                }
                .to_owned(),
            )
        });
        AttemptRecord {
            sequence: attempt.sequence,
            channel: attempt.channel.as_str().to_owned(),
            stage: attempt.stage,
            attempt: attempt.attempt,
            outcome: attempt.outcome.clone(),
            target_hash: attempt.target_hash.clone(),
            provider_status: attempt.provider_status,
            provider_request_id: attempt.provider_request_id.clone(),
            provider_acknowledged_at_ms: delivered_outbox
                .and_then(|outbox| outbox.provider_acknowledged_at_ms),
            provider_expired_at_ms: delivered_outbox
                .and_then(|outbox| outbox.provider_expired_at_ms),
            provider_acknowledged_by_hash: delivered_outbox
                .and_then(|outbox| outbox.provider_acknowledged_by_hash.clone()),
            provider_acknowledgement_state,
            reason: attempt.reason.clone(),
            attempted_at_ms: attempt.attempted_at_ms,
            retry_at_ms: attempt.retry_at_ms,
        }
    }

    fn update_receipt(
        &self,
        logical_id: &str,
        principal_id: &str,
        update: ReceiptUpdate,
        now_ms: i64,
        audit_action: &str,
    ) -> anyhow::Result<()> {
        {
            let mut state = self.lock_state();
            let authorized = state
                .logical
                .get(logical_id)
                .is_some_and(|logical| logical.owner_id == principal_id);
            if !authorized {
                return Err(RuleStoreError::NotAuthorized.into());
            }
            let receipt = state
                .receipts
                .get_mut(&(logical_id.to_owned(), principal_id.to_owned()))
                .ok_or(RuleStoreError::NotAuthorized)?;
            match update {
                ReceiptUpdate::Seen => receipt.seen_at_ms.get_or_insert(now_ms),
                ReceiptUpdate::Acknowledged => receipt.acknowledged_at_ms.get_or_insert(now_ms),
                ReceiptUpdate::Cleared => receipt.cleared_at_ms.insert(now_ms),
            };
            state.prune(now_ms)?;
        }
        self.record_inbox_audit(principal_id, audit_action, logical_id, None, now_ms);
        Ok(())
    }

    fn clear_scope_matches(
        state: &RuntimeState,
        logical_id: &str,
        principal_id: &str,
        scope: &ClearScope,
    ) -> bool {
        match scope {
            ClearScope::All => true,
            ClearScope::Rule(rule_id) => state.logical.get(logical_id).is_some_and(|logical| {
                logical.owner_id == principal_id && logical.rule_id == *rule_id
            }),
            ClearScope::Before(before_ms) => state.logical.get(logical_id).is_some_and(|logical| {
                logical.owner_id == principal_id && logical.updated_at_ms < *before_ms
            }),
        }
    }

    fn record_inbox_audit(
        &self,
        principal_id: &str,
        action: &str,
        subject_id: &str,
        detail: Option<&str>,
        now_ms: i64,
    ) {
        tracing::info!(
            event = "notification_audit",
            principal_id,
            action,
            subject_id,
            detail = detail.unwrap_or(""),
            occurred_at_ms = now_ms
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::notifications::{Lifecycle, state::PendingHistoryEntry};

    fn test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("keeppeek-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn seed_notification(store: &Store, logical_id: &str, rule_id: &str, updated_at_ms: i64) {
        let mut state = store.lock_state();
        state.logical.insert(
            logical_id.to_owned(),
            LogicalNotification {
                id: logical_id.to_owned(),
                rule_id: rule_id.to_owned(),
                owner_id: "owner-1".to_owned(),
                source_id: "front-door".to_owned(),
                source_identity: logical_id.to_owned(),
                lifecycle: Lifecycle::Event,
                stage: Stage::Preliminary,
                highest_revision: 1,
                enrichment_attempts: 0,
                created_at_ms: updated_at_ms,
                updated_at_ms,
                enrichment_deadline_at_ms: updated_at_ms.saturating_add(10_000),
                title: "Person".to_owned(),
                body: "Detected".to_owned(),
                deep_link: "/events/test".to_owned(),
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
        state.push_history(PendingHistoryEntry {
            logical_id,
            rule_id,
            revision: 1,
            stage: Stage::Preliminary,
            outcome: "created",
            reason: None,
            occurred_at_ms: updated_at_ms,
            next_eligible_at_ms: None,
        });
    }

    #[test]
    fn receipts_are_principal_scoped_and_independent() {
        let directory = test_dir("notification-inbox-receipts");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        seed_notification(&store, "logical-1", "rule-1", 1_000);
        seed_notification(&store, "logical-2", "rule-2", 2_000);

        let inbox = store.inbox("owner-1", 100).unwrap();
        assert_eq!(inbox.unread_count, 2);
        assert_eq!(inbox.items[0].logical_id, "logical-2");

        store.mark_seen("logical-1", "owner-1", 3_000).unwrap();
        assert_eq!(store.inbox("owner-1", 100).unwrap().unread_count, 1);
        assert!(
            store
                .mark_seen("logical-2", "another-owner", 3_000)
                .unwrap_err()
                .downcast_ref::<RuleStoreError>()
                .is_some()
        );

        store.clear("logical-1", "owner-1", 4_000).unwrap();
        let remaining = store.inbox("owner-1", 100).unwrap();
        assert_eq!(remaining.items.len(), 1);
        assert_eq!(remaining.items[0].logical_id, "logical-2");
        store.acknowledge("logical-2", "owner-1", 5_000).unwrap();
        assert_eq!(
            store
                .clear_scope("owner-1", &ClearScope::All, 6_000)
                .unwrap(),
            1
        );
        assert!(store.inbox("owner-1", 100).unwrap().items.is_empty());

        let history = store.history("owner-1", 100).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|group| group.events.len() == 1));
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn scoped_clear_only_changes_the_selected_rule() {
        let directory = test_dir("notification-inbox-scope");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        seed_notification(&store, "logical-1", "rule-1", 1_000);
        seed_notification(&store, "logical-2", "rule-2", 2_000);

        assert_eq!(
            store
                .clear_scope("owner-1", &ClearScope::Rule("rule-1".to_owned()), 3_000,)
                .unwrap(),
            1
        );
        let inbox = store.inbox("owner-1", 100).unwrap();
        assert_eq!(inbox.items.len(), 1);
        assert_eq!(inbox.items[0].rule_id, "rule-2");
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
