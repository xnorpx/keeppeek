use crate::storage::metadata::EventAttachment;
use serde::Serialize;
use std::sync::atomic::Ordering;

use super::{
    Lifecycle, LogicalId, ProcessSummary, RulePolicy, RuleStoreError, Stage, Transition,
    cooldown_keys, logical_id,
    model::{
        Action, AttachmentPolicy, Candidate, Channel, MatchResult, RateLimit, RateLimitScope, Rule,
        Severity, Trigger,
    },
    state::{
        InboxReceipt, LogicalNotification, MAX_OPERATIONAL_INTERVALS, MAX_PENDING_OUTBOX,
        OperationalInterval, OperationalIntervalKey, OutboxItem, OutboxStatus, PendingHistoryEntry,
        RateWindow, RuntimeState,
    },
    store::Store,
};

#[derive(Debug)]
struct StoredLogical {
    stage: Stage,
    highest_revision: u64,
    enrichment_attempts: u32,
    enrichment_deadline_at_ms: i64,
}

#[derive(Serialize)]
struct Destination<'a> {
    value: &'a str,
}

#[derive(Serialize)]
struct Payload<'a> {
    title: &'a str,
    body: &'a str,
    deep_link: &'a str,
    occurred_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    event_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    canonical_attachment: Option<&'a EventAttachment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_key: Option<&'a str>,
    image_availability: &'static str,
    source: PayloadSource<'a>,
    event: PayloadEvent<'a>,
}

#[derive(Serialize)]
struct PayloadSource<'a> {
    id: &'a str,
    name: Option<&'a str>,
    group_ids: &'a [String],
}

#[derive(Serialize)]
struct PayloadEvent<'a> {
    id: &'a str,
    revision: u64,
    kind: Option<&'a str>,
    lifecycle: &'static str,
    stage: &'static str,
    duration_ms: Option<u64>,
    severity: &'static str,
    recovered: bool,
    payload: Option<&'a serde_json::Value>,
}

struct EnqueueContext<'a> {
    logical_id: &'a LogicalId,
    default_title: &'a str,
    default_body: &'a str,
    attachment_path: Option<&'a str>,
    replacement: bool,
}

struct AvailableAttachment {
    path: String,
    byte_len: u64,
}

enum NewNotificationAdmission {
    Accepted {
        critical_bypass: bool,
    },
    Suppressed {
        reason: &'static str,
        next_eligible_at_ms: Option<i64>,
    },
}

impl Store {
    pub(super) fn process(&self, mut candidate: Candidate) -> anyhow::Result<ProcessSummary> {
        let rules = self.resolved_active_rules()?;
        let mut matches = Vec::with_capacity(rules.len());
        for rule in rules {
            matches.push((rule.matches(&candidate)?, rule));
        }
        let attachment = matches
            .iter()
            .any(|(result, _)| *result == MatchResult::Match)
            .then(|| available_attachment(&candidate))
            .flatten();
        let mut state = self.lock_state();
        state.prune(candidate.occurred_at_ms)?;
        Self::normalize_operational_interval(&mut state, &mut candidate)?;
        let summary = self.process_in_state(&mut state, &candidate, matches, attachment.as_ref());
        state.prune(candidate.occurred_at_ms)?;
        drop(state);
        self.record_process_metrics(summary);
        Ok(summary)
    }

    fn normalize_operational_interval(
        state: &mut RuntimeState,
        candidate: &mut Candidate,
    ) -> anyhow::Result<()> {
        if is_tracked_operational_event(candidate) {
            return Ok(());
        }
        let starts_interval = matches!(
            candidate.trigger,
            Trigger::OutageStarted | Trigger::StorageHealth | Trigger::RecordingHealth
        );
        let closes_interval = candidate.trigger == Trigger::Recovery
            && matches!(
                candidate.lifecycle,
                Lifecycle::Outage | Lifecycle::Storage | Lifecycle::Recording
            );
        if !starts_interval && !closes_interval {
            return Ok(());
        }
        let key = OperationalIntervalKey {
            source_id: candidate.source_id.clone(),
            lifecycle: candidate.lifecycle,
            event_kind: candidate.event_kind.clone().unwrap_or_default(),
        };
        let existing = state.operational_intervals.get(&key).cloned();
        if starts_interval {
            if let Some(existing) = existing.filter(|interval| interval.active) {
                candidate.source_identity = existing.identity;
                candidate.revision = existing.revision;
                return Ok(());
            }
            if !state.operational_intervals.contains_key(&key)
                && state.operational_intervals.len() >= MAX_OPERATIONAL_INTERVALS
            {
                anyhow::bail!("notification process-local operational interval limit reached");
            }
            candidate.revision = 1;
            state.operational_intervals.insert(
                key,
                OperationalInterval {
                    identity: candidate.source_identity.clone(),
                    revision: 1,
                    active: true,
                    updated_at_ms: candidate.occurred_at_ms,
                },
            );
        } else if let Some(existing) = existing.filter(|interval| interval.active) {
            candidate.source_identity = existing.identity;
            candidate.revision = existing.revision.saturating_add(1);
            if let Some(interval) = state.operational_intervals.get_mut(&key) {
                interval.revision = candidate.revision;
                interval.active = false;
                interval.updated_at_ms = candidate.occurred_at_ms;
            }
        }
        Ok(())
    }

    pub(super) fn test_rule(
        &self,
        rule_id: &str,
        owner_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<ProcessSummary> {
        let record = self
            .lock_state()
            .rules
            .get(rule_id)
            .cloned()
            .ok_or(RuleStoreError::NotFound)?;
        if record.owner_id != owner_id {
            return Err(RuleStoreError::NotAuthorized.into());
        }
        let rule = record
            .active
            .ok_or_else(|| anyhow::anyhow!("notification rule is not active"))?;
        let mut rule = self.resolve_rule_destinations(&rule)?;
        rule.validate()?;
        rule.cooldowns.clear();
        let identity = format!("notification-test-{}", uuid::Uuid::new_v4());
        let candidate = Candidate {
            trigger: Trigger::Test,
            source_id: rule
                .filter
                .source_ids
                .first()
                .cloned()
                .unwrap_or_else(|| "notification-test".to_owned()),
            source_name: None,
            source_identity: identity.clone(),
            lifecycle: Lifecycle::Test,
            event_kind: rule
                .filter
                .event_kinds
                .first()
                .cloned()
                .or_else(|| Some("test".to_owned())),
            payload: None,
            group_ids: Vec::new(),
            zone: rule.filter.zones.first().cloned(),
            confidence: rule.filter.minimum_confidence.or(Some(1.0)),
            attachment_path: None,
            canonical_attachment: None,
            icon_key: Some("event".to_owned()),
            image_available: false,
            duration_ms: rule.filter.minimum_duration_ms,
            severity: Severity::Info,
            reviewed: rule.filter.reviewed,
            bookmarked: rule.filter.bookmarked,
            privacy_active: false,
            revision: 1,
            stage: Stage::Preliminary,
            occurred_at_ms: now_ms,
            deep_link: format!("/settings#notifications-{identity}"),
        };
        let mut summary = ProcessSummary {
            matched: 1,
            ..ProcessSummary::default()
        };
        let mut state = self.lock_state();
        state.prune(now_ms)?;
        self.process_rule(&mut state, &rule, &candidate, None, &mut summary);
        state.prune(now_ms)?;
        drop(state);
        self.record_process_metrics(summary);
        Ok(summary)
    }

    fn resolved_active_rules(&self) -> anyhow::Result<Vec<Rule>> {
        let rules = self
            .lock_state()
            .rules
            .values()
            .filter_map(|record| record.active.clone())
            .collect::<Vec<_>>();
        rules
            .iter()
            .map(|rule| self.resolve_rule_destinations(rule))
            .collect()
    }

    fn process_in_state(
        &self,
        state: &mut RuntimeState,
        candidate: &Candidate,
        matches: Vec<(MatchResult, Rule)>,
        attachment: Option<&AvailableAttachment>,
    ) -> ProcessSummary {
        let mut summary = ProcessSummary::default();
        for (result, rule) in matches {
            match result {
                MatchResult::NoMatch => continue,
                MatchResult::Suppressed(reason) => {
                    summary.matched = summary.matched.saturating_add(1);
                    summary.suppressed = summary.suppressed.saturating_add(1);
                    let logical_id = candidate_logical_id(&rule, candidate);
                    Self::record_history(
                        state,
                        history_entry(
                            &logical_id,
                            &rule.id,
                            candidate,
                            "suppressed",
                            Some(reason.as_str()),
                            None,
                        ),
                    );
                }
                MatchResult::Match => {
                    summary.matched = summary.matched.saturating_add(1);
                    self.process_rule(state, &rule, candidate, attachment, &mut summary);
                }
            }
        }
        summary
    }

    fn process_rule(
        &self,
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        attachment: Option<&AvailableAttachment>,
        summary: &mut ProcessSummary,
    ) {
        let logical_id = candidate_logical_id(rule, candidate);
        let attachment_path = usable_attachment(rule, attachment);
        if let Some(existing) = Self::logical(state, &logical_id) {
            Self::process_existing(
                state,
                rule,
                candidate,
                &logical_id,
                existing,
                attachment_path,
                summary,
            );
            return;
        }
        let critical_bypass = match Self::admit_new_notification(state, rule, candidate) {
            NewNotificationAdmission::Accepted { critical_bypass } => critical_bypass,
            NewNotificationAdmission::Suppressed {
                reason,
                next_eligible_at_ms,
            } => {
                summary.suppressed = summary.suppressed.saturating_add(1);
                Self::record_history(
                    state,
                    history_entry(
                        &logical_id,
                        &rule.id,
                        candidate,
                        "suppressed",
                        Some(reason),
                        next_eligible_at_ms,
                    ),
                );
                return;
            }
        };
        if critical_bypass {
            self.record_audit(
                &rule.owner_id,
                "critical_bypass",
                logical_id.as_str(),
                candidate.occurred_at_ms,
                Some(candidate.revision),
            );
        }
        let queued =
            Self::create_new_notification(state, rule, candidate, &logical_id, attachment_path);
        summary.created = summary.created.saturating_add(1);
        summary.queued_attempts = summary.queued_attempts.saturating_add(queued);
    }

    fn create_new_notification(
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        logical_id: &LogicalId,
        attachment_path: Option<String>,
    ) -> u32 {
        let (title, body) = render_logical(rule, candidate);
        Self::insert_new_logical(
            state,
            rule,
            candidate,
            logical_id,
            &title,
            &body,
            attachment_path.as_deref(),
        );
        Self::enqueue_actions(
            state,
            rule,
            candidate,
            EnqueueContext {
                logical_id,
                default_title: &title,
                default_body: &body,
                attachment_path: attachment_path.as_deref(),
                replacement: false,
            },
        )
    }

    fn admit_new_notification(
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
    ) -> NewNotificationAdmission {
        let quiet_bypass = candidate.severity == Severity::Critical
            && rule
                .schedule
                .status_at(candidate.occurred_at_ms)
                .expect("an active notification rule must have a valid schedule")
                .quiet;
        let cooldown_eligible_at = Self::cooldown_eligible_at(state, rule, candidate);
        let rate_limited = Self::rule_rate_limited(state, rule, candidate);
        let critical_bypass = quiet_bypass || cooldown_eligible_at.is_some() || rate_limited;
        if critical_bypass {
            let bypass_available = candidate.severity == Severity::Critical
                && rule.critical_bypass.is_some()
                && Self::critical_bypass_available(state, rule, candidate);
            if !bypass_available {
                let reason = if quiet_bypass {
                    "critical_bypass_limited"
                } else if cooldown_eligible_at.is_some() {
                    "cooldown"
                } else {
                    "rate_limited"
                };
                return NewNotificationAdmission::Suppressed {
                    reason,
                    next_eligible_at_ms: cooldown_eligible_at,
                };
            }
        }
        if !state.logical_capacity_available() {
            return NewNotificationAdmission::Suppressed {
                reason: "logical_state_full",
                next_eligible_at_ms: None,
            };
        }
        if !Self::start_cooldowns(state, rule, candidate) {
            return NewNotificationAdmission::Suppressed {
                reason: "cooldown_state_full",
                next_eligible_at_ms: None,
            };
        }
        if critical_bypass {
            Self::consume_critical_bypass(state, rule, candidate);
        }
        NewNotificationAdmission::Accepted { critical_bypass }
    }

    fn insert_new_logical(
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        logical_id: &LogicalId,
        title: &str,
        body: &str,
        attachment_path: Option<&str>,
    ) {
        state.logical.insert(
            logical_id.as_str().to_owned(),
            LogicalNotification {
                id: logical_id.as_str().to_owned(),
                rule_id: rule.id.clone(),
                owner_id: rule.owner_id.clone(),
                source_id: candidate.source_id.clone(),
                source_identity: candidate.source_identity.clone(),
                lifecycle: candidate.lifecycle,
                stage: candidate.stage,
                highest_revision: candidate.revision,
                enrichment_attempts: 0,
                created_at_ms: candidate.occurred_at_ms,
                updated_at_ms: candidate.occurred_at_ms,
                enrichment_deadline_at_ms: add_millis(
                    candidate.occurred_at_ms,
                    rule.enrichment.deadline_ms,
                ),
                title: title.to_owned(),
                body: body.to_owned(),
                deep_link: candidate.deep_link.clone(),
                attachment_path: attachment_path.map(str::to_owned),
                severity: candidate.severity,
                canonical_attachment: candidate.canonical_attachment.clone(),
                icon_key: candidate.icon_key.clone(),
                image_available: candidate.image_available,
            },
        );
        state.receipts.insert(
            (logical_id.as_str().to_owned(), rule.owner_id.clone()),
            InboxReceipt::default(),
        );
        Self::consume_rule_rate_limits(state, rule, candidate);
        if let Some(record) = state.rules.get_mut(&rule.id) {
            record.last_match_at_ms = Some(candidate.occurred_at_ms);
        }
        Self::record_history(
            state,
            history_entry(logical_id, &rule.id, candidate, "created", None, None),
        );
    }

    fn process_existing(
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        logical_id: &LogicalId,
        existing: StoredLogical,
        attachment_path: Option<String>,
        summary: &mut ProcessSummary,
    ) {
        if candidate.revision <= existing.highest_revision {
            Self::record_existing_suppression(
                state,
                rule,
                candidate,
                logical_id,
                "duplicate_revision",
                summary,
            );
            return;
        }

        let tracked_operational_event = is_tracked_operational_event(candidate);
        if !tracked_operational_event
            && candidate.stage == Stage::Enriched
            && candidate.revision > u64::from(rule.enrichment.maximum_revisions)
        {
            let logical = state
                .logical
                .get_mut(logical_id.as_str())
                .expect("an existing notification must remain in state");
            logical.highest_revision = candidate.revision;
            logical.updated_at_ms = candidate.occurred_at_ms;
            Self::record_existing_suppression(
                state,
                rule,
                candidate,
                logical_id,
                "enrichment_revision_limit",
                summary,
            );
            return;
        }

        let replace = candidate.stage == Stage::Recovery
            || (candidate.stage == Stage::Enriched
                && (existing.stage == Stage::Preliminary || tracked_operational_event));
        let late = !tracked_operational_event
            && candidate.stage == Stage::Enriched
            && candidate.occurred_at_ms > existing.enrichment_deadline_at_ms;
        let attempts_exhausted = !tracked_operational_event
            && candidate.stage == Stage::Enriched
            && existing.enrichment_attempts >= rule.enrichment.maximum_attempts;
        if !replace || attempts_exhausted || (late && !rule.enrichment.wake_after_deadline) {
            Self::update_collapsed_logical(state, logical_id, candidate, attachment_path);
            let reason = if late {
                "late_enrichment"
            } else if attempts_exhausted {
                "enrichment_attempt_limit"
            } else {
                "revision_collapsed"
            };
            Self::record_existing_suppression(state, rule, candidate, logical_id, reason, summary);
            return;
        }

        let queued = Self::replace_existing(
            state,
            rule,
            candidate,
            logical_id,
            attachment_path,
            tracked_operational_event,
            late,
        );
        summary.replaced = summary.replaced.saturating_add(1);
        summary.queued_attempts = summary.queued_attempts.saturating_add(queued);
    }

    fn record_existing_suppression(
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        logical_id: &LogicalId,
        reason: &str,
        summary: &mut ProcessSummary,
    ) {
        summary.suppressed = summary.suppressed.saturating_add(1);
        Self::record_history(
            state,
            history_entry(
                logical_id,
                &rule.id,
                candidate,
                "collapsed",
                Some(reason),
                None,
            ),
        );
    }

    fn update_collapsed_logical(
        state: &mut RuntimeState,
        logical_id: &LogicalId,
        candidate: &Candidate,
        attachment_path: Option<String>,
    ) {
        let logical = state
            .logical
            .get_mut(logical_id.as_str())
            .expect("an existing notification must remain in state");
        logical.highest_revision = candidate.revision;
        logical.updated_at_ms = candidate.occurred_at_ms;
        logical.canonical_attachment = candidate.canonical_attachment.clone();
        logical.icon_key = candidate.icon_key.clone();
        logical.image_available = candidate.image_available;
        logical.attachment_path = attachment_path;
    }

    fn replace_existing(
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        logical_id: &LogicalId,
        attachment_path: Option<String>,
        tracked_operational_event: bool,
        late: bool,
    ) -> u32 {
        let (title, body) = render_logical(rule, candidate);
        let logical = state
            .logical
            .get_mut(logical_id.as_str())
            .expect("an existing notification must remain in state");
        logical.stage = candidate.stage;
        logical.highest_revision = candidate.revision;
        if candidate.stage == Stage::Enriched && !tracked_operational_event {
            logical.enrichment_attempts = logical.enrichment_attempts.saturating_add(1);
        }
        logical.updated_at_ms = candidate.occurred_at_ms;
        logical.title.clone_from(&title);
        logical.body.clone_from(&body);
        logical.deep_link.clone_from(&candidate.deep_link);
        logical.attachment_path.clone_from(&attachment_path);
        logical.severity = candidate.severity;
        logical
            .canonical_attachment
            .clone_from(&candidate.canonical_attachment);
        logical.icon_key.clone_from(&candidate.icon_key);
        logical.image_available = candidate.image_available;
        Self::record_history(
            state,
            history_entry(
                logical_id,
                &rule.id,
                candidate,
                "replaced",
                late.then_some("late_enrichment_configured"),
                None,
            ),
        );
        Self::enqueue_actions(
            state,
            rule,
            candidate,
            EnqueueContext {
                logical_id,
                default_title: &title,
                default_body: &body,
                attachment_path: attachment_path.as_deref(),
                replacement: true,
            },
        )
    }

    fn logical(state: &RuntimeState, logical_id: &LogicalId) -> Option<StoredLogical> {
        state
            .logical
            .get(logical_id.as_str())
            .map(|logical| StoredLogical {
                stage: logical.stage,
                highest_revision: logical.highest_revision,
                enrichment_attempts: logical.enrichment_attempts,
                enrichment_deadline_at_ms: logical.enrichment_deadline_at_ms,
            })
    }

    fn cooldown_eligible_at(
        state: &RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
    ) -> Option<i64> {
        let mut next = None;
        for (key, _) in cooldown_keys(&policy(rule), &transition(candidate)) {
            if let Some(&eligible_at_ms) = state.cooldowns.get(&key)
                && candidate.occurred_at_ms < eligible_at_ms
            {
                next = Some(next.map_or(eligible_at_ms, |value: i64| value.max(eligible_at_ms)));
            }
        }
        next
    }

    fn start_cooldowns(state: &mut RuntimeState, rule: &Rule, candidate: &Candidate) -> bool {
        let cooldowns = cooldown_keys(&policy(rule), &transition(candidate));
        let keys = cooldowns
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        if !state.cooldown_capacity_for(&keys) {
            return false;
        }
        for (key, duration_ms) in cooldowns {
            state
                .cooldowns
                .insert(key, add_millis(candidate.occurred_at_ms, duration_ms));
        }
        true
    }

    fn rule_rate_limited(state: &RuntimeState, rule: &Rule, candidate: &Candidate) -> bool {
        for limit in &rule.rate_limits {
            if limit.scope != RateLimitScope::Channel
                && Self::rate_limited(state, limit, rule, candidate, None)
            {
                return true;
            }
        }
        false
    }

    fn consume_rule_rate_limits(state: &mut RuntimeState, rule: &Rule, candidate: &Candidate) {
        for limit in &rule.rate_limits {
            if limit.scope != RateLimitScope::Channel {
                Self::consume_rate(state, limit, rule, candidate, None);
            }
        }
    }

    fn rate_limited(
        state: &RuntimeState,
        limit: &RateLimit,
        rule: &Rule,
        candidate: &Candidate,
        channel: Option<Channel>,
    ) -> bool {
        let (scope, value) = rate_key(limit.scope, rule, candidate, channel);
        Self::rate_limited_for_key(state, limit, &scope, &value, candidate.occurred_at_ms)
    }

    fn consume_rate(
        state: &mut RuntimeState,
        limit: &RateLimit,
        rule: &Rule,
        candidate: &Candidate,
        channel: Option<Channel>,
    ) {
        let (scope, value) = rate_key(limit.scope, rule, candidate, channel);
        Self::consume_rate_for_key(
            state,
            limit.window_ms,
            &scope,
            &value,
            candidate.occurred_at_ms,
        );
    }

    fn critical_bypass_available(state: &RuntimeState, rule: &Rule, candidate: &Candidate) -> bool {
        let bypass = rule
            .critical_bypass
            .as_ref()
            .expect("critical bypass policy must exist after availability check");
        let limit = RateLimit {
            scope: RateLimitScope::Rule,
            maximum: bypass.maximum,
            window_ms: bypass.window_ms,
        };
        !Self::rate_limited_for_key(
            state,
            &limit,
            "critical_bypass",
            &rule.id,
            candidate.occurred_at_ms,
        )
    }

    fn consume_critical_bypass(state: &mut RuntimeState, rule: &Rule, candidate: &Candidate) {
        let bypass = rule
            .critical_bypass
            .as_ref()
            .expect("critical bypass policy must exist after availability check");
        Self::consume_rate_for_key(
            state,
            bypass.window_ms,
            "critical_bypass",
            &rule.id,
            candidate.occurred_at_ms,
        );
    }

    fn rate_limited_for_key(
        state: &RuntimeState,
        limit: &RateLimit,
        scope: &str,
        value: &str,
        now_ms: i64,
    ) -> bool {
        let Some(window) = state
            .rate_windows
            .get(&(scope.to_owned(), value.to_owned()))
        else {
            return false;
        };
        now_ms < add_millis(window.started_at_ms, limit.window_ms)
            && window.count >= u64::from(limit.maximum)
    }

    fn consume_rate_for_key(
        state: &mut RuntimeState,
        window_ms: u64,
        scope: &str,
        value: &str,
        now_ms: i64,
    ) {
        let key = (scope.to_owned(), value.to_owned());
        let window = state.rate_windows.entry(key).or_insert(RateWindow {
            started_at_ms: now_ms,
            count: 0,
        });
        if now_ms >= add_millis(window.started_at_ms, window_ms) {
            window.started_at_ms = now_ms;
            window.count = 1;
        } else {
            window.count = window.count.saturating_add(1);
        }
    }

    fn enqueue_actions(
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        context: EnqueueContext<'_>,
    ) -> u32 {
        let mut queued = 0_u32;
        for (index, action) in rule.actions.iter().enumerate() {
            if Self::enqueue_action(state, rule, candidate, &context, index, action) {
                queued = queued.saturating_add(1);
            }
        }
        queued
    }

    fn enqueue_action(
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        context: &EnqueueContext<'_>,
        index: usize,
        action: &Action,
    ) -> bool {
        let rejection = if !action.enabled {
            return false;
        } else if context.replacement
            && !supports_replacement(action.channel)
            && !action.allow_second_delivery
        {
            Some(("collapsed", "replacement_unsupported"))
        } else if action.attachment == AttachmentPolicy::Required
            && context.attachment_path.is_none()
        {
            Some(("failed", "attachment_required"))
        } else if Self::channel_rate_limited(state, rule, candidate, action.channel) {
            Some(("rate_limited", action.channel.as_str()))
        } else if state.pending_outbox_count() >= MAX_PENDING_OUTBOX {
            Some(("expired", "outbox_full"))
        } else {
            None
        };
        if let Some((outcome, reason)) = rejection {
            Self::record_history(
                state,
                history_entry(
                    context.logical_id,
                    &rule.id,
                    candidate,
                    outcome,
                    Some(reason),
                    None,
                ),
            );
            return false;
        }
        if state.outbox_key_exists(context.logical_id.as_str(), index, candidate.stage) {
            return false;
        }
        let id = state.next_outbox_id();
        state.outbox.insert(
            id,
            Self::build_outbox_item(id, rule, candidate, context, index, action),
        );
        Self::consume_channel_rate_limits(state, rule, candidate, action.channel);
        true
    }

    fn build_outbox_item(
        id: u64,
        rule: &Rule,
        candidate: &Candidate,
        context: &EnqueueContext<'_>,
        index: usize,
        action: &Action,
    ) -> OutboxItem {
        let rendered_title = render(&action.template.title, candidate);
        let rendered_body = render(&action.template.body, candidate);
        let title = if rendered_title.is_empty() {
            context.default_title
        } else {
            &rendered_title
        };
        let body = if rendered_body.is_empty() {
            context.default_body
        } else {
            &rendered_body
        };
        let destination_json = serde_json::to_string(&Destination {
            value: &action.destination,
        })
        .expect("a notification destination string must serialize");
        let payload_json = Self::serialize_payload(candidate, title, body);
        OutboxItem {
            id,
            logical_id: context.logical_id.as_str().to_owned(),
            action_index: index,
            stage: candidate.stage,
            channel: action.channel,
            destination_json,
            payload_json,
            replacement_key: context.logical_id.as_str().to_owned(),
            priority: if candidate.severity == Severity::Critical {
                100
            } else {
                0
            },
            status: OutboxStatus::Pending,
            attempt_count: 0,
            max_attempts: rule.failure.maximum_attempts,
            max_retry_interval_ms: rule.failure.maximum_retry_interval_ms,
            attachment_enabled: action.attachment != AttachmentPolicy::Never,
            attachment_required: action.attachment == AttachmentPolicy::Required,
            max_attachment_bytes: rule.enrichment.maximum_attachment_bytes,
            next_attempt_at_ms: candidate.occurred_at_ms,
            expires_at_ms: add_millis(candidate.occurred_at_ms, rule.failure.expiry_ms),
            updated_at_ms: candidate.occurred_at_ms,
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

    fn serialize_payload(candidate: &Candidate, title: &str, body: &str) -> String {
        serde_json::to_string(&Payload {
            title,
            body,
            deep_link: &candidate.deep_link,
            occurred_at_ms: candidate.occurred_at_ms,
            event_id: (candidate.lifecycle == Lifecycle::Event)
                .then_some(candidate.source_identity.as_str()),
            event_revision: (candidate.lifecycle == Lifecycle::Event).then_some(candidate.revision),
            canonical_attachment: candidate.canonical_attachment.as_ref(),
            icon_key: candidate.icon_key.as_deref(),
            image_availability: candidate_image_availability(candidate),
            source: PayloadSource {
                id: &candidate.source_id,
                name: candidate.source_name.as_deref(),
                group_ids: &candidate.group_ids,
            },
            event: PayloadEvent {
                id: &candidate.source_identity,
                revision: candidate.revision,
                kind: candidate.event_kind.as_deref(),
                lifecycle: candidate.lifecycle.as_str(),
                stage: stage_str(candidate.stage),
                duration_ms: candidate.duration_ms,
                severity: candidate.severity.as_str(),
                recovered: candidate.stage == Stage::Recovery,
                payload: candidate.payload.as_ref(),
            },
        })
        .expect("a validated notification payload must serialize")
    }

    fn record_process_metrics(&self, summary: ProcessSummary) {
        self.metrics
            .pending_deliveries
            .fetch_add(u64::from(summary.queued_attempts), Ordering::Relaxed);
        self.metrics
            .notifications_created
            .fetch_add(u64::from(summary.created), Ordering::Relaxed);
        self.metrics
            .notifications_replaced
            .fetch_add(u64::from(summary.replaced), Ordering::Relaxed);
        self.metrics
            .notifications_suppressed
            .fetch_add(u64::from(summary.suppressed), Ordering::Relaxed);
    }

    fn channel_rate_limited(
        state: &RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        channel: Channel,
    ) -> bool {
        for limit in &rule.rate_limits {
            if limit.scope == RateLimitScope::Channel
                && Self::rate_limited(state, limit, rule, candidate, Some(channel))
            {
                return true;
            }
        }
        false
    }

    fn consume_channel_rate_limits(
        state: &mut RuntimeState,
        rule: &Rule,
        candidate: &Candidate,
        channel: Channel,
    ) {
        for limit in &rule.rate_limits {
            if limit.scope == RateLimitScope::Channel {
                Self::consume_rate(state, limit, rule, candidate, Some(channel));
            }
        }
    }

    fn record_history(state: &mut RuntimeState, entry: PendingHistoryEntry<'_>) {
        state.push_history(entry);
    }

    fn record_audit(
        &self,
        principal_id: &str,
        action: &str,
        subject_id: &str,
        occurred_at_ms: i64,
        revision: Option<u64>,
    ) {
        tracing::info!(
            event = "notification_audit",
            principal_id,
            action,
            subject_id,
            occurred_at_ms,
            revision
        );
    }
}

const fn candidate_image_availability(candidate: &Candidate) -> &'static str {
    if candidate.canonical_attachment.is_none() {
        "none"
    } else if candidate.image_available {
        "available"
    } else {
        "unavailable"
    }
}

fn candidate_logical_id(rule: &Rule, candidate: &Candidate) -> LogicalId {
    logical_id(&policy(rule), &transition(candidate))
}

fn history_entry<'a>(
    logical_id: &'a LogicalId,
    rule_id: &'a str,
    candidate: &'a Candidate,
    outcome: &'a str,
    reason: Option<&'a str>,
    next_eligible_at_ms: Option<i64>,
) -> PendingHistoryEntry<'a> {
    PendingHistoryEntry {
        logical_id: logical_id.as_str(),
        rule_id,
        revision: candidate.revision,
        stage: candidate.stage,
        outcome,
        reason,
        occurred_at_ms: candidate.occurred_at_ms,
        next_eligible_at_ms,
    }
}

fn is_tracked_operational_event(candidate: &Candidate) -> bool {
    candidate.event_kind.as_deref().is_some_and(|kind| {
        matches!(
            kind,
            "camera_offline" | "stream_stale" | "decode_unavailable" | "recording_interrupted"
        )
    })
}

fn policy(rule: &Rule) -> RulePolicy {
    RulePolicy {
        rule_id: rule.id.clone(),
        cooldowns: rule.cooldowns.clone(),
    }
}

fn transition(candidate: &Candidate) -> Transition {
    Transition {
        source_id: candidate.source_id.clone(),
        identity: candidate.source_identity.clone(),
        lifecycle: candidate.lifecycle,
        event_kind: candidate.event_kind.clone(),
        group_ids: candidate.group_ids.clone(),
    }
}

fn rate_key(
    scope: RateLimitScope,
    rule: &Rule,
    candidate: &Candidate,
    channel: Option<Channel>,
) -> (String, String) {
    let prefix = if candidate.trigger == Trigger::Test {
        "test_"
    } else {
        ""
    };
    match scope {
        RateLimitScope::Rule => (format!("{prefix}rule"), rule.id.clone()),
        RateLimitScope::Channel => (
            format!("{prefix}channel"),
            channel.map(Channel::as_str).unwrap_or("unknown").to_owned(),
        ),
        RateLimitScope::Principal => (format!("{prefix}principal"), rule.owner_id.clone()),
        RateLimitScope::Global => (format!("{prefix}global"), "*".to_owned()),
    }
}

fn render_logical(rule: &Rule, candidate: &Candidate) -> (String, String) {
    let title = rule
        .actions
        .first()
        .map(|action| render(&action.template.title, candidate))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| rule.name.clone());
    let body = rule
        .actions
        .first()
        .map(|action| render(&action.template.body, candidate))
        .unwrap_or_default();
    (title, body)
}

fn render(template: &str, candidate: &Candidate) -> String {
    let confidence = candidate
        .confidence
        .map(|value| format!("{value:.2}"))
        .unwrap_or_default();
    let duration = candidate
        .duration_ms
        .map(|value| value.to_string())
        .unwrap_or_default();
    let values = [
        ("source.id", candidate.source_id.as_str()),
        (
            "source.name",
            candidate.source_name.as_deref().unwrap_or(""),
        ),
        ("event.id", candidate.source_identity.as_str()),
        ("event.kind", candidate.event_kind.as_deref().unwrap_or("")),
        ("event.zone", candidate.zone.as_deref().unwrap_or("")),
        ("event.confidence", confidence.as_str()),
        ("event.duration", duration.as_str()),
        (
            "health.state",
            candidate.event_kind.as_deref().unwrap_or(""),
        ),
        ("notification.stage", stage_str(candidate.stage)),
        ("notification.deep_link", candidate.deep_link.as_str()),
    ];
    let mut rendered = template.to_owned();
    for (field, value) in values {
        rendered = rendered.replace(&format!("{{{{{field}}}}}"), value);
    }
    rendered
}

fn available_attachment(candidate: &Candidate) -> Option<AvailableAttachment> {
    if candidate.privacy_active {
        return None;
    }
    let path = candidate.attachment_path.as_ref()?;
    let metadata = std::fs::metadata(path).ok()?;
    Some(AvailableAttachment {
        path: path.clone(),
        byte_len: metadata.len(),
    })
}

fn usable_attachment(rule: &Rule, attachment: Option<&AvailableAttachment>) -> Option<String> {
    attachment
        .filter(|attachment| attachment.byte_len <= rule.enrichment.maximum_attachment_bytes)
        .map(|attachment| attachment.path.clone())
}

const fn supports_replacement(channel: Channel) -> bool {
    matches!(channel, Channel::Browser | Channel::Push)
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
    use std::path::PathBuf;

    use super::*;
    use crate::notifications::{
        Cooldown, CooldownScope,
        model::{
            CriticalBypass, EnrichmentPolicy, FailurePolicy, Filter, QuietHours, Schedule,
            Template, Weekday, WeeklyWindow,
        },
    };

    fn test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("keeppeek-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn rule(cooldown_scope: CooldownScope) -> Rule {
        Rule {
            id: "person-alert".to_owned(),
            name: "Person alert".to_owned(),
            enabled: true,
            revision: 0,
            owner_id: "owner-1".to_owned(),
            triggers: vec![Trigger::EventCreated, Trigger::EventUpdated, Trigger::Test],
            filter: Filter {
                event_kinds: vec!["person".to_owned()],
                ..Filter::default()
            },
            schedule: Schedule {
                timezone: "UTC".to_owned(),
                active_windows: Vec::new(),
                quiet_hours: None,
            },
            cooldowns: vec![Cooldown {
                scope: cooldown_scope,
                duration_ms: 30_000,
            }],
            rate_limits: Vec::new(),
            critical_bypass: None,
            enrichment: EnrichmentPolicy {
                deadline_ms: 10_000,
                maximum_revisions: 4,
                maximum_attempts: 2,
                maximum_attachment_bytes: 1_048_576,
                wake_after_deadline: false,
            },
            actions: vec![
                Action {
                    enabled: true,
                    channel: Channel::Browser,
                    destination: String::new(),
                    template: Template {
                        title: "{{event.kind}} at {{source.id}}".to_owned(),
                        body: "Open {{notification.deep_link}}".to_owned(),
                    },
                    attachment: AttachmentPolicy::WhenAvailable,
                    allow_second_delivery: false,
                },
                Action {
                    enabled: true,
                    channel: Channel::Webhook,
                    destination: "https://example.invalid/keeppeek".to_owned(),
                    template: Template {
                        title: "{{event.kind}}".to_owned(),
                        body: "{{event.id}}".to_owned(),
                    },
                    attachment: AttachmentPolicy::Never,
                    allow_second_delivery: false,
                },
            ],
            failure: FailurePolicy {
                maximum_attempts: 3,
                maximum_retry_interval_ms: 60_000,
                expiry_ms: 3_600_000,
            },
        }
    }

    fn candidate(
        trigger: Trigger,
        source_id: &str,
        identity: &str,
        revision: u64,
        stage: Stage,
        occurred_at_ms: i64,
    ) -> Candidate {
        Candidate {
            trigger,
            source_id: source_id.to_owned(),
            source_name: None,
            source_identity: identity.to_owned(),
            lifecycle: if trigger == Trigger::Test {
                Lifecycle::Test
            } else {
                Lifecycle::Event
            },
            event_kind: Some("person".to_owned()),
            payload: None,
            group_ids: Vec::new(),
            zone: None,
            confidence: Some(0.9),
            attachment_path: None,
            canonical_attachment: None,
            icon_key: Some("person".to_owned()),
            image_available: false,
            duration_ms: None,
            severity: Severity::Info,
            reviewed: Some(false),
            bookmarked: Some(false),
            privacy_active: false,
            revision,
            stage,
            occurred_at_ms,
            deep_link: format!("/events/{identity}"),
        }
    }

    fn activate(store: &Store, rule: Rule) {
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
    }

    fn history_count(store: &Store, outcome: Option<&str>, reasons: &[&str]) -> usize {
        store
            .lock_state()
            .history
            .iter()
            .filter(|entry| {
                outcome.is_none_or(|outcome| entry.outcome == outcome)
                    && (reasons.is_empty()
                        || entry
                            .reason
                            .as_deref()
                            .is_some_and(|reason| reasons.contains(&reason)))
            })
            .count()
    }

    #[test]
    fn stages_and_retries_share_identity_without_cross_camera_collapse() {
        let directory = test_dir("notification-orchestration");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        activate(&store, rule(CooldownScope::Event));

        let preliminary = candidate(
            Trigger::EventCreated,
            "front-door",
            "event-1",
            1,
            Stage::Preliminary,
            1_000,
        );
        assert_eq!(
            store.process(preliminary.clone()).unwrap(),
            ProcessSummary {
                matched: 1,
                created: 1,
                queued_attempts: 2,
                ..ProcessSummary::default()
            }
        );
        assert_eq!(store.process(preliminary).unwrap().suppressed, 1);

        let mut enriched = candidate(
            Trigger::EventUpdated,
            "front-door",
            "event-1",
            2,
            Stage::Enriched,
            5_000,
        );
        enriched.canonical_attachment = Some(EventAttachment {
            id: "snapshot-1".to_owned(),
            attachment_type: "snapshot".to_owned(),
            content_type: "image/jpeg".to_owned(),
            byte_len: Some(100),
            ordinal: 0,
            timestamp_ms: Some(4_500),
            text: None,
        });
        enriched.icon_key = Some("person".to_owned());
        let enriched_summary = store.process(enriched).unwrap();
        assert_eq!(enriched_summary.replaced, 1);
        assert_eq!(enriched_summary.queued_attempts, 1);

        let mut collapsed = candidate(
            Trigger::EventUpdated,
            "front-door",
            "event-1",
            3,
            Stage::Enriched,
            6_000,
        );
        collapsed.canonical_attachment = Some(EventAttachment {
            id: "snapshot-2".to_owned(),
            attachment_type: "snapshot".to_owned(),
            content_type: "image/webp".to_owned(),
            byte_len: Some(80),
            ordinal: 0,
            timestamp_ms: Some(5_500),
            text: None,
        });
        collapsed.icon_key = Some("person".to_owned());
        assert_eq!(store.process(collapsed).unwrap().suppressed, 1);
        let inbox = store.inbox("owner-1", 10).unwrap();
        let notification = inbox
            .items
            .iter()
            .find(|item| item.source_id == "front-door")
            .unwrap();
        assert_eq!(notification.source_identity, "event-1");
        assert_eq!(notification.revision, 3);
        assert_eq!(
            notification
                .canonical_attachment
                .as_ref()
                .map(|attachment| attachment.id.as_str()),
            Some("snapshot-2")
        );
        assert_eq!(notification.icon_key.as_deref(), Some("person"));
        assert!(!notification.image_available);

        let unrelated = candidate(
            Trigger::EventCreated,
            "back-door",
            "event-1",
            1,
            Stage::Preliminary,
            2_000,
        );
        assert_eq!(store.process(unrelated).unwrap().created, 1);
        assert_eq!(store.lock_state().logical.len(), 2);
        assert_eq!(store.lock_state().outbox.len(), 5);
        assert_eq!(history_count(&store, None, &["replacement_unsupported"]), 1);
        let metrics = store.metrics.snapshot();
        assert_eq!(metrics.configured_rules, 1);
        assert_eq!(metrics.pending_deliveries, 5);
        assert_eq!(metrics.notifications_created, 2);
        assert_eq!(metrics.notifications_replaced, 1);
        assert_eq!(metrics.notifications_suppressed, 2);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cooldown_resets_when_the_process_store_restarts() {
        let directory = test_dir("notification-cooldown-restart");
        let config_path = directory.join("config.toml");
        let store = Store::open(&config_path).unwrap();
        activate(&store, rule(CooldownScope::CameraEventKind));
        assert_eq!(
            store
                .process(candidate(
                    Trigger::EventCreated,
                    "front-door",
                    "event-1",
                    1,
                    Stage::Preliminary,
                    1_000,
                ))
                .unwrap()
                .created,
            1
        );
        drop(store);

        let reopened = Store::open(&config_path).unwrap();
        assert_eq!(
            reopened
                .process(candidate(
                    Trigger::EventCreated,
                    "front-door",
                    "event-2",
                    1,
                    Stage::Preliminary,
                    30_999,
                ))
                .unwrap()
                .created,
            1
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn test_notifications_use_separate_rate_accounting() {
        let directory = test_dir("notification-test-rate");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let mut configured = rule(CooldownScope::Event);
        configured.rate_limits = vec![RateLimit {
            scope: RateLimitScope::Rule,
            maximum: 1,
            window_ms: 60_000,
        }];
        activate(&store, configured);

        assert_eq!(
            store
                .process(candidate(
                    Trigger::EventCreated,
                    "front-door",
                    "event-1",
                    1,
                    Stage::Preliminary,
                    1_000,
                ))
                .unwrap()
                .created,
            1
        );
        assert_eq!(
            store
                .process(candidate(
                    Trigger::EventCreated,
                    "front-door",
                    "event-2",
                    1,
                    Stage::Preliminary,
                    2_000,
                ))
                .unwrap()
                .suppressed,
            1
        );
        assert_eq!(
            store
                .process(candidate(
                    Trigger::Test,
                    "front-door",
                    "test-1",
                    1,
                    Stage::Preliminary,
                    3_000,
                ))
                .unwrap()
                .created,
            1
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn recovery_replaces_the_original_outage_during_cooldown() {
        let directory = test_dir("notification-outage-recovery");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let mut configured = rule(CooldownScope::Outage);
        configured.triggers = vec![Trigger::OutageStarted, Trigger::Recovery];
        configured.filter.event_kinds = vec!["camera_outage".to_owned()];
        configured.actions.truncate(1);
        activate(&store, configured);

        let mut outage = candidate(
            Trigger::OutageStarted,
            "front-door",
            "outage-1",
            1,
            Stage::Preliminary,
            1_000,
        );
        outage.lifecycle = Lifecycle::Outage;
        outage.event_kind = Some("camera_outage".to_owned());
        assert_eq!(store.process(outage.clone()).unwrap().created, 1);

        let mut replayed_after_restart = outage.clone();
        replayed_after_restart.source_identity = "different-process-identity".to_owned();
        replayed_after_restart.occurred_at_ms = 1_500;
        assert_eq!(store.process(replayed_after_restart).unwrap().suppressed, 1);

        let mut recovery = outage;
        recovery.trigger = Trigger::Recovery;
        recovery.revision = 2;
        recovery.stage = Stage::Recovery;
        recovery.occurred_at_ms = 2_000;
        let summary = store.process(recovery).unwrap();
        assert_eq!(summary.replaced, 1);
        assert_eq!(summary.queued_attempts, 1);
        assert_eq!(store.lock_state().logical.len(), 1);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn operational_revisions_match_duration_and_deduplicate_replays() {
        let directory = test_dir("notification-operational-revisions");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let mut configured = rule(CooldownScope::Outage);
        configured.triggers = vec![
            Trigger::OutageStarted,
            Trigger::EventUpdated,
            Trigger::Recovery,
        ];
        configured.filter.event_kinds = vec!["stream_stale".to_owned()];
        configured.filter.minimum_duration_ms = Some(10_000);
        configured.actions.truncate(1);
        activate(&store, configured);

        let mut started = candidate(
            Trigger::OutageStarted,
            "front-door",
            "operational-1",
            1,
            Stage::Preliminary,
            1_000,
        );
        started.lifecycle = Lifecycle::Outage;
        started.event_kind = Some("stream_stale".to_owned());
        started.payload = Some(serde_json::json!({
            "cause": "frames_not_arriving",
            "affected_streams": ["main"],
            "recording_interrupted": true,
            "evidence_source": "canonical_health",
        }));
        started.duration_ms = Some(10_000);
        started.severity = Severity::Warning;
        assert_eq!(store.process(started.clone()).unwrap().created, 1);
        assert_eq!(store.process(started.clone()).unwrap().suppressed, 1);

        let mut updated = started.clone();
        updated.trigger = Trigger::EventUpdated;
        updated.revision = 10;
        updated.stage = Stage::Enriched;
        updated.occurred_at_ms = 31_000;
        updated.duration_ms = Some(30_000);
        updated.severity = Severity::Critical;
        assert_eq!(store.process(updated.clone()).unwrap().replaced, 1);
        assert_eq!(store.process(updated).unwrap().suppressed, 1);

        let mut recovered = started;
        recovered.trigger = Trigger::Recovery;
        recovered.revision = 11;
        recovered.stage = Stage::Recovery;
        recovered.occurred_at_ms = 61_000;
        recovered.duration_ms = Some(60_000);
        recovered.severity = Severity::Critical;
        assert_eq!(store.process(recovered.clone()).unwrap().replaced, 1);
        assert_eq!(store.process(recovered).unwrap().suppressed, 1);

        assert_eq!(store.lock_state().logical.len(), 1);
        assert_eq!(store.lock_state().outbox.len(), 3);
        let payload_json = store
            .lock_state()
            .outbox
            .values()
            .find(|item| item.stage == Stage::Recovery)
            .unwrap()
            .payload_json
            .clone();
        let payload: serde_json::Value = serde_json::from_str(&payload_json).unwrap();
        assert_eq!(payload["source"]["id"], "front-door");
        assert_eq!(payload["event"]["id"], "operational-1");
        assert_eq!(payload["event"]["revision"], 11);
        assert_eq!(payload["event"]["kind"], "stream_stale");
        assert_eq!(payload["event"]["lifecycle"], "outage");
        assert_eq!(payload["event"]["stage"], "recovery");
        assert_eq!(payload["event"]["duration_ms"], 60_000);
        assert_eq!(payload["event"]["severity"], "critical");
        assert_eq!(payload["event"]["recovered"], true);
        assert_eq!(payload["event"]["payload"]["cause"], "frames_not_arriving");
        assert_eq!(payload["event"]["payload"]["affected_streams"][0], "main");
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn late_enrichment_and_required_attachment_are_visible() {
        let directory = test_dir("notification-late-attachment");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let mut configured = rule(CooldownScope::Event);
        configured.actions.truncate(1);
        configured.actions[0].attachment = AttachmentPolicy::Required;
        configured.enrichment.deadline_ms = 1_000;
        activate(&store, configured);

        let preliminary = candidate(
            Trigger::EventCreated,
            "front-door",
            "event-1",
            1,
            Stage::Preliminary,
            1_000,
        );
        let first = store.process(preliminary).unwrap();
        assert_eq!(first.created, 1);
        assert_eq!(first.queued_attempts, 0);

        let enriched = candidate(
            Trigger::EventUpdated,
            "front-door",
            "event-1",
            2,
            Stage::Enriched,
            2_001,
        );
        assert_eq!(store.process(enriched).unwrap().suppressed, 1);
        assert_eq!(
            history_count(&store, None, &["attachment_required", "late_enrichment"]),
            2
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn critical_bypass_is_bounded() {
        let directory = test_dir("notification-critical-bypass");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let mut configured = rule(CooldownScope::CameraEventKind);
        configured.actions.truncate(1);
        configured.schedule.quiet_hours = Some(QuietHours {
            windows: vec![WeeklyWindow {
                weekdays: vec![
                    Weekday::Monday,
                    Weekday::Tuesday,
                    Weekday::Wednesday,
                    Weekday::Thursday,
                    Weekday::Friday,
                    Weekday::Saturday,
                    Weekday::Sunday,
                ],
                start_minute: 0,
                end_minute: 1_439,
            }],
        });
        configured.critical_bypass = Some(CriticalBypass {
            maximum: 1,
            window_ms: 60_000,
        });
        activate(&store, configured);

        let mut first = candidate(
            Trigger::EventCreated,
            "front-door",
            "event-1",
            1,
            Stage::Preliminary,
            1_000,
        );
        first.severity = Severity::Critical;
        assert_eq!(store.process(first).unwrap().created, 1);
        let mut second = candidate(
            Trigger::EventCreated,
            "front-door",
            "event-2",
            1,
            Stage::Preliminary,
            2_000,
        );
        second.severity = Severity::Critical;
        assert_eq!(store.process(second).unwrap().suppressed, 1);
        assert_eq!(history_count(&store, None, &["critical_bypass_limited"]), 1);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn attachment_outbox_defers_bytes_without_disclosing_local_path() {
        let directory = test_dir("notification-attachment-payload");
        let image_path = directory.join("event-1.jpg");
        std::fs::write(&image_path, [1_u8, 2, 3, 4]).unwrap();
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let mut configured = rule(CooldownScope::Event);
        configured.actions.truncate(1);
        activate(&store, configured);

        let mut event = candidate(
            Trigger::EventCreated,
            "front-door",
            "event-1",
            1,
            Stage::Preliminary,
            1_000,
        );
        event.attachment_path = Some(image_path.to_string_lossy().into_owned());
        assert_eq!(store.process(event).unwrap().queued_attempts, 1);
        let item = store.lock_state().outbox.values().next().cloned().unwrap();
        let payload = item.payload_json;
        assert!(!payload.contains("AQIDBA=="));
        assert!(!payload.contains(image_path.to_string_lossy().as_ref()));
        assert!(item.attachment_enabled);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn disabled_action_preserves_notification_history_without_enqueueing_delivery() {
        let directory = test_dir("notification-disabled-action");
        let store = Store::open(&directory.join("config.toml")).unwrap();
        let mut configured = rule(CooldownScope::Event);
        configured.actions.truncate(1);
        configured.cooldowns.clear();
        activate(&store, configured.clone());

        let first = store
            .process(candidate(
                Trigger::EventCreated,
                "front-door",
                "event-1",
                1,
                Stage::Preliminary,
                1_000,
            ))
            .unwrap();
        assert_eq!(first.queued_attempts, 1);

        let current = store.rules("owner-1").unwrap().remove(0);
        configured.actions[0].enabled = false;
        let saved = store
            .save_draft(configured, current.draft_revision, 1_500)
            .unwrap();
        store
            .activate(
                &saved.id,
                &saved.owner_id,
                saved.active_revision,
                saved.draft_revision,
                1_600,
            )
            .unwrap();
        assert_eq!(store.metrics.snapshot().pending_deliveries, 0);
        let second = store
            .process(candidate(
                Trigger::EventCreated,
                "front-door",
                "event-2",
                1,
                Stage::Preliminary,
                2_000,
            ))
            .unwrap();

        assert_eq!(second.created, 1);
        assert_eq!(second.queued_attempts, 0);
        let state = store.lock_state();
        assert_eq!(state.outbox.len(), 1);
        assert_eq!(
            state.outbox.values().next().unwrap().status,
            OutboxStatus::Expired
        );
        drop(state);
        assert_eq!(
            history_count(&store, Some("expired"), &["rule_or_action_disabled"]),
            1
        );
        assert_eq!(history_count(&store, Some("created"), &[]), 2);
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
