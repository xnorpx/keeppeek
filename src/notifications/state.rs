use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::storage::metadata::EventAttachment;

use super::{
    CooldownKey, Lifecycle, RuleRecord, Stage,
    model::{Channel, Rule, Severity},
};

pub(super) const MAX_PENDING_OUTBOX: usize = 10_000;
const MAX_TERMINAL_NOTIFICATIONS: usize = 2_000;
const MAX_LOGICAL_NOTIFICATIONS: usize = MAX_PENDING_OUTBOX + MAX_TERMINAL_NOTIFICATIONS;
pub(super) const MAX_HISTORY_EVENTS: usize = 50_000;
const MAX_DELIVERY_ATTEMPTS: usize = 50_000;
pub(super) const MAX_COOLDOWN_WINDOWS: usize = 10_000;
const MAX_RATE_WINDOWS: usize = 4_096;
const MAX_INACTIVE_OPERATIONAL_INTERVALS: usize = 4_096;
pub(super) const MAX_OPERATIONAL_INTERVALS: usize = 8_192;

#[derive(Debug, Clone)]
pub(super) struct LogicalNotification {
    pub(super) id: String,
    pub(super) rule_id: String,
    pub(super) owner_id: String,
    pub(super) source_id: String,
    pub(super) source_identity: String,
    pub(super) lifecycle: Lifecycle,
    pub(super) stage: Stage,
    pub(super) highest_revision: u64,
    pub(super) enrichment_attempts: u32,
    pub(super) created_at_ms: i64,
    pub(super) updated_at_ms: i64,
    pub(super) enrichment_deadline_at_ms: i64,
    pub(super) title: String,
    pub(super) body: String,
    pub(super) deep_link: String,
    pub(super) attachment_path: Option<String>,
    pub(super) severity: Severity,
    pub(super) canonical_attachment: Option<EventAttachment>,
    pub(super) icon_key: Option<String>,
    pub(super) image_available: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct InboxReceipt {
    pub(super) seen_at_ms: Option<i64>,
    pub(super) acknowledged_at_ms: Option<i64>,
    pub(super) cleared_at_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub(super) struct HistoryEntry {
    pub(super) sequence: u64,
    pub(super) logical_id: String,
    pub(super) rule_id: String,
    pub(super) revision: u64,
    pub(super) stage: Stage,
    pub(super) outcome: String,
    pub(super) reason: Option<String>,
    pub(super) occurred_at_ms: i64,
    pub(super) next_eligible_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct OperationalIntervalKey {
    pub(super) source_id: String,
    pub(super) lifecycle: Lifecycle,
    pub(super) event_kind: String,
}

#[derive(Debug, Clone)]
pub(super) struct OperationalInterval {
    pub(super) identity: String,
    pub(super) revision: u64,
    pub(super) active: bool,
    pub(super) updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub(super) struct RateWindow {
    pub(super) started_at_ms: i64,
    pub(super) count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutboxStatus {
    Pending,
    Retrying,
    Delivering,
    Delivered,
    Failed,
    Expired,
}

impl OutboxStatus {
    pub(super) const fn pending(self) -> bool {
        matches!(self, Self::Pending | Self::Retrying)
    }
}

#[derive(Debug, Clone)]
pub(super) struct OutboxItem {
    pub(super) id: u64,
    pub(super) logical_id: String,
    pub(super) action_index: usize,
    pub(super) stage: Stage,
    pub(super) channel: Channel,
    pub(super) destination_json: String,
    pub(super) payload_json: String,
    pub(super) replacement_key: String,
    pub(super) priority: i32,
    pub(super) status: OutboxStatus,
    pub(super) attempt_count: u32,
    pub(super) max_attempts: u32,
    pub(super) max_retry_interval_ms: u64,
    pub(super) attachment_enabled: bool,
    pub(super) attachment_required: bool,
    pub(super) max_attachment_bytes: u64,
    pub(super) next_attempt_at_ms: i64,
    pub(super) expires_at_ms: i64,
    pub(super) updated_at_ms: i64,
    pub(super) last_reason: Option<String>,
    pub(super) provider_request_id: Option<String>,
    pub(super) provider_receipt: Option<String>,
    pub(super) next_receipt_check_at_ms: Option<i64>,
    pub(super) provider_receipt_expires_at_ms: Option<i64>,
    pub(super) provider_acknowledged_at_ms: Option<i64>,
    pub(super) provider_expired_at_ms: Option<i64>,
    pub(super) provider_acknowledged_by_hash: Option<String>,
}

impl OutboxItem {
    pub(super) fn receipt_pending(&self) -> bool {
        self.status == OutboxStatus::Delivered
            && self.provider_receipt.is_some()
            && self.provider_acknowledged_at_ms.is_none()
            && self.provider_expired_at_ms.is_none()
            && self.next_receipt_check_at_ms.is_some()
    }
}

#[derive(Debug, Clone)]
pub(super) struct DeliveryAttempt {
    pub(super) sequence: u64,
    pub(super) outbox_id: u64,
    pub(super) logical_id: String,
    pub(super) channel: Channel,
    pub(super) stage: Stage,
    pub(super) attempt: u32,
    pub(super) outcome: String,
    pub(super) target_hash: String,
    pub(super) provider_status: Option<u16>,
    pub(super) provider_request_id: Option<String>,
    pub(super) reason: Option<String>,
    pub(super) attempted_at_ms: i64,
    pub(super) retry_at_ms: Option<i64>,
}

#[derive(Debug)]
pub(super) struct RuntimeState {
    pub(super) rules: BTreeMap<String, RuleRecord>,
    pub(super) logical: HashMap<String, LogicalNotification>,
    pub(super) operational_intervals: HashMap<OperationalIntervalKey, OperationalInterval>,
    pub(super) receipts: HashMap<(String, String), InboxReceipt>,
    pub(super) history: VecDeque<HistoryEntry>,
    pub(super) cooldowns: HashMap<CooldownKey, i64>,
    pub(super) rate_windows: HashMap<(String, String), RateWindow>,
    pub(super) outbox: BTreeMap<u64, OutboxItem>,
    pub(super) attempts: VecDeque<DeliveryAttempt>,
    next_history_sequence: u64,
    next_attempt_sequence: u64,
    next_outbox_id: u64,
}

impl RuntimeState {
    pub(super) fn new(rules: BTreeMap<String, RuleRecord>) -> Self {
        Self {
            rules,
            logical: HashMap::new(),
            operational_intervals: HashMap::new(),
            receipts: HashMap::new(),
            history: VecDeque::new(),
            cooldowns: HashMap::new(),
            rate_windows: HashMap::new(),
            outbox: BTreeMap::new(),
            attempts: VecDeque::new(),
            next_history_sequence: 1,
            next_attempt_sequence: 1,
            next_outbox_id: 1,
        }
    }

    pub(super) const fn next_outbox_id(&mut self) -> u64 {
        let id = self.next_outbox_id;
        self.next_outbox_id = self
            .next_outbox_id
            .checked_add(1)
            .expect("notification outbox ID sequence must not overflow");
        id
    }

    pub(super) fn pending_outbox_count(&self) -> usize {
        self.outbox
            .values()
            .filter(|item| item.status.pending())
            .count()
    }

    pub(super) fn logical_capacity_available(&self) -> bool {
        self.logical.len() < MAX_LOGICAL_NOTIFICATIONS
    }

    pub(super) fn cooldown_capacity_for(&self, keys: &[CooldownKey]) -> bool {
        let additional = keys
            .iter()
            .filter(|key| !self.cooldowns.contains_key(*key))
            .collect::<HashSet<_>>()
            .len();
        self.cooldowns.len().saturating_add(additional) <= MAX_COOLDOWN_WINDOWS
    }

    pub(super) fn outbox_key_exists(
        &self,
        logical_id: &str,
        action_index: usize,
        stage: Stage,
    ) -> bool {
        self.outbox.values().any(|item| {
            item.logical_id == logical_id
                && item.action_index == action_index
                && item.stage == stage
        })
    }

    pub(super) fn push_history(&mut self, entry: PendingHistoryEntry<'_>) {
        tracing::info!(
            event = "notification_outcome",
            logical_id = entry.logical_id,
            rule_id = entry.rule_id,
            revision = entry.revision,
            stage = stage_str(entry.stage),
            outcome = entry.outcome,
            reason = entry.reason.unwrap_or(""),
            occurred_at_ms = entry.occurred_at_ms,
            next_eligible_at_ms = entry.next_eligible_at_ms
        );
        let sequence = self.next_history_sequence;
        self.next_history_sequence = self
            .next_history_sequence
            .checked_add(1)
            .expect("notification history sequence must not overflow");
        self.history.push_back(HistoryEntry {
            sequence,
            logical_id: entry.logical_id.to_owned(),
            rule_id: entry.rule_id.to_owned(),
            revision: entry.revision,
            stage: entry.stage,
            outcome: entry.outcome.to_owned(),
            reason: entry.reason.map(str::to_owned),
            occurred_at_ms: entry.occurred_at_ms,
            next_eligible_at_ms: entry.next_eligible_at_ms,
        });
        while self.history.len() > MAX_HISTORY_EVENTS {
            self.history.pop_front();
        }
    }

    pub(super) fn push_attempt(&mut self, mut attempt: DeliveryAttempt) {
        attempt.sequence = self.next_attempt_sequence;
        self.next_attempt_sequence = self
            .next_attempt_sequence
            .checked_add(1)
            .expect("notification attempt sequence must not overflow");
        self.attempts.push_back(attempt);
        while self.attempts.len() > MAX_DELIVERY_ATTEMPTS {
            self.attempts.pop_front();
        }
    }

    pub(super) fn prune(&mut self, now_ms: i64) -> anyhow::Result<()> {
        self.cooldowns
            .retain(|_, eligible_at_ms| *eligible_at_ms > now_ms);
        if self.cooldowns.len() > MAX_COOLDOWN_WINDOWS {
            anyhow::bail!("notification process-local cooldown limit reached");
        }
        prune_oldest_rate_windows(&mut self.rate_windows);
        prune_operational_intervals(&mut self.operational_intervals)?;
        self.prune_terminal_notifications();
        if self.logical.len() > MAX_LOGICAL_NOTIFICATIONS {
            anyhow::bail!("notification process-local logical notification limit reached");
        }
        Ok(())
    }

    fn prune_terminal_notifications(&mut self) {
        let active_logical_ids = self
            .outbox
            .values()
            .filter(|item| {
                item.status.pending()
                    || item.status == OutboxStatus::Delivering
                    || item.receipt_pending()
            })
            .map(|item| item.logical_id.clone())
            .collect::<HashSet<_>>();
        let mut terminal = self
            .logical
            .values()
            .filter(|logical| !active_logical_ids.contains(&logical.id))
            .map(|logical| (logical.updated_at_ms, logical.id.clone()))
            .collect::<Vec<_>>();
        terminal.sort_unstable_by(|left, right| right.cmp(left));
        let removed = terminal
            .into_iter()
            .skip(MAX_TERMINAL_NOTIFICATIONS)
            .map(|(_, logical_id)| logical_id)
            .collect::<HashSet<_>>();
        if removed.is_empty() {
            return;
        }
        self.logical
            .retain(|logical_id, _| !removed.contains(logical_id));
        self.receipts
            .retain(|(logical_id, _), _| !removed.contains(logical_id));
        self.history
            .retain(|entry| !removed.contains(&entry.logical_id));
        self.attempts
            .retain(|entry| !removed.contains(&entry.logical_id));
        self.outbox
            .retain(|_, item| !removed.contains(&item.logical_id));
    }

    pub(super) fn cancel_disabled_outbox(
        &mut self,
        rule_id: &str,
        active: Option<&Rule>,
        now_ms: i64,
    ) -> u64 {
        let mut cancelled = Vec::new();
        for item in self
            .outbox
            .values_mut()
            .filter(|item| item.status.pending())
        {
            let Some(logical) = self.logical.get(&item.logical_id) else {
                continue;
            };
            if logical.rule_id != rule_id {
                continue;
            }
            let allowed = active.is_some_and(|rule| {
                rule.enabled
                    && rule
                        .actions
                        .get(item.action_index)
                        .is_some_and(|action| action.enabled && action.channel == item.channel)
            });
            if !allowed {
                item.status = OutboxStatus::Expired;
                item.updated_at_ms = now_ms;
                item.last_reason = Some("rule_or_action_disabled".to_owned());
                cancelled.push((item.logical_id.clone(), item.stage));
            }
        }
        for (logical_id, stage) in &cancelled {
            if let Some(logical) = self.logical.get(logical_id) {
                self.push_history(PendingHistoryEntry {
                    logical_id,
                    rule_id,
                    revision: logical.highest_revision,
                    stage: *stage,
                    outcome: "expired",
                    reason: Some("rule_or_action_disabled"),
                    occurred_at_ms: now_ms,
                    next_eligible_at_ms: None,
                });
            }
        }
        u64::try_from(cancelled.len()).unwrap_or(u64::MAX)
    }
}

const fn stage_str(stage: Stage) -> &'static str {
    match stage {
        Stage::Preliminary => "preliminary",
        Stage::Enriched => "enriched",
        Stage::Recovery => "recovery",
    }
}

pub(super) struct PendingHistoryEntry<'a> {
    pub(super) logical_id: &'a str,
    pub(super) rule_id: &'a str,
    pub(super) revision: u64,
    pub(super) stage: Stage,
    pub(super) outcome: &'a str,
    pub(super) reason: Option<&'a str>,
    pub(super) occurred_at_ms: i64,
    pub(super) next_eligible_at_ms: Option<i64>,
}

fn prune_oldest_rate_windows(windows: &mut HashMap<(String, String), RateWindow>) {
    if windows.len() <= MAX_RATE_WINDOWS {
        return;
    }
    let mut oldest = windows
        .iter()
        .map(|(key, window)| (window.started_at_ms, key.clone()))
        .collect::<Vec<_>>();
    oldest.sort_unstable();
    for (_, key) in oldest.into_iter().take(windows.len() - MAX_RATE_WINDOWS) {
        windows.remove(&key);
    }
}

fn prune_operational_intervals(
    intervals: &mut HashMap<OperationalIntervalKey, OperationalInterval>,
) -> anyhow::Result<()> {
    let mut inactive = intervals
        .iter()
        .filter(|(_, interval)| !interval.active)
        .map(|(key, interval)| (interval.updated_at_ms, key.clone()))
        .collect::<Vec<_>>();
    inactive.sort_unstable_by_key(|entry| entry.0);
    let remove_count = inactive
        .len()
        .saturating_sub(MAX_INACTIVE_OPERATIONAL_INTERVALS);
    for (_, key) in inactive.into_iter().take(remove_count) {
        intervals.remove(&key);
    }
    if intervals.len() > MAX_OPERATIONAL_INTERVALS {
        anyhow::bail!("notification process-local operational interval limit reached");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::CooldownScope;

    fn logical(id: String, updated_at_ms: i64) -> LogicalNotification {
        LogicalNotification {
            id: id.clone(),
            rule_id: "rule-1".to_owned(),
            owner_id: "owner-1".to_owned(),
            source_id: "front-door".to_owned(),
            source_identity: id,
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
        }
    }

    fn outbox(id: u64, logical_id: String, status: OutboxStatus) -> OutboxItem {
        OutboxItem {
            id,
            logical_id: logical_id.clone(),
            action_index: 0,
            stage: Stage::Preliminary,
            channel: Channel::Browser,
            destination_json: r#"{"value":""}"#.to_owned(),
            payload_json: r#"{"title":"Person"}"#.to_owned(),
            replacement_key: logical_id,
            priority: 0,
            status,
            attempt_count: 0,
            max_attempts: 3,
            max_retry_interval_ms: 5_000,
            attachment_enabled: false,
            attachment_required: false,
            max_attachment_bytes: 1_048_576,
            next_attempt_at_ms: 1_000,
            expires_at_ms: 60_000,
            updated_at_ms: 1_000,
            last_reason: None,
            provider_request_id: None,
            provider_receipt: None,
            next_receipt_check_at_ms: None,
            provider_receipt_expires_at_ms: None,
            provider_acknowledged_at_ms: None,
            provider_expired_at_ms: None,
            provider_acknowledged_by_hash: None,
        }
    }

    #[test]
    fn duplicate_cooldown_keys_consume_one_capacity_slot() {
        let mut state = RuntimeState::new(BTreeMap::new());
        for index in 0..MAX_COOLDOWN_WINDOWS - 1 {
            state.cooldowns.insert(
                CooldownKey {
                    rule_id: "rule-1".to_owned(),
                    scope: CooldownScope::Group,
                    value: format!("group-{index}"),
                },
                10_000,
            );
        }
        let final_key = CooldownKey {
            rule_id: "rule-1".to_owned(),
            scope: CooldownScope::Group,
            value: "last-group".to_owned(),
        };

        assert!(state.cooldown_capacity_for(&[final_key.clone(), final_key]));
    }

    #[test]
    fn terminal_pruning_keeps_active_and_newest_notifications() {
        let mut state = RuntimeState::new(BTreeMap::new());
        for index in 0..=MAX_TERMINAL_NOTIFICATIONS {
            let logical_id = format!("terminal-{index:04}");
            state.logical.insert(
                logical_id.clone(),
                logical(logical_id.clone(), i64::try_from(index).unwrap()),
            );
            state.receipts.insert(
                (logical_id.clone(), "owner-1".to_owned()),
                InboxReceipt::default(),
            );
            state.push_history(PendingHistoryEntry {
                logical_id: &logical_id,
                rule_id: "rule-1",
                revision: 1,
                stage: Stage::Preliminary,
                outcome: "created",
                reason: None,
                occurred_at_ms: i64::try_from(index).unwrap(),
                next_eligible_at_ms: None,
            });
        }
        let removed_id = "terminal-0000";
        let removed_outbox_id = state.next_outbox_id();
        state.outbox.insert(
            removed_outbox_id,
            outbox(
                removed_outbox_id,
                removed_id.to_owned(),
                OutboxStatus::Delivered,
            ),
        );
        state.push_attempt(DeliveryAttempt {
            sequence: 0,
            outbox_id: removed_outbox_id,
            logical_id: removed_id.to_owned(),
            channel: Channel::Browser,
            stage: Stage::Preliminary,
            attempt: 1,
            outcome: "delivered".to_owned(),
            target_hash: "hash".to_owned(),
            provider_status: None,
            provider_request_id: None,
            reason: None,
            attempted_at_ms: 1_000,
            retry_at_ms: None,
        });
        let active_id = "active".to_owned();
        state
            .logical
            .insert(active_id.clone(), logical(active_id.clone(), -1));
        let active_outbox_id = state.next_outbox_id();
        state.outbox.insert(
            active_outbox_id,
            outbox(active_outbox_id, active_id.clone(), OutboxStatus::Pending),
        );

        state.prune(0).unwrap();

        assert_eq!(state.logical.len(), MAX_TERMINAL_NOTIFICATIONS + 1);
        assert!(!state.logical.contains_key(removed_id));
        assert!(state.logical.contains_key("terminal-2000"));
        assert!(state.logical.contains_key(&active_id));
        assert!(
            !state
                .receipts
                .contains_key(&(removed_id.to_owned(), "owner-1".to_owned()))
        );
        assert!(
            !state
                .history
                .iter()
                .any(|entry| entry.logical_id == removed_id)
        );
        assert!(
            !state
                .attempts
                .iter()
                .any(|attempt| attempt.logical_id == removed_id)
        );
        assert!(!state.outbox.contains_key(&removed_outbox_id));
        assert!(state.outbox.contains_key(&active_outbox_id));
    }
}
