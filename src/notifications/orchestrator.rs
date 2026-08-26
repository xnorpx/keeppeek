use serde::Serialize;

use super::{
    Lifecycle, LogicalId, ProcessSummary, RulePolicy, RuleStoreError, Stage, Transition,
    cooldown_keys, logical_id,
    model::{
        AttachmentPolicy, Candidate, Channel, MatchResult, RateLimit, RateLimitScope, Rule,
        Severity, Trigger,
    },
    store::Store,
};

const MAX_PENDING_OUTBOX: u64 = 10_000;

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
}

struct EnqueueContext<'a> {
    logical_id: &'a LogicalId,
    default_title: &'a str,
    default_body: &'a str,
    attachment_path: Option<&'a str>,
    replacement: bool,
}

impl Store {
    pub(super) fn process(&self, mut candidate: Candidate) -> anyhow::Result<ProcessSummary> {
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                self.normalize_operational_interval(&mut candidate).await?;
                self.process_in_transaction(&candidate).await
            }
            .await;
            match result {
                Ok(summary) => {
                    self.connection.execute_batch("COMMIT").await?;
                    Ok(summary)
                }
                Err(error) => {
                    let _ = self.connection.execute_batch("ROLLBACK").await;
                    Err(error)
                }
            }
        })
    }

    async fn normalize_operational_interval(
        &self,
        candidate: &mut Candidate,
    ) -> anyhow::Result<()> {
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
        let event_kind = candidate.event_kind.as_deref().unwrap_or("");
        let mut rows = self
            .connection
            .query(
                "SELECT identity, revision, active
                 FROM notification_operational_intervals
                 WHERE source_id = ?1 AND lifecycle = ?2 AND event_kind = ?3",
                turso::params![
                    candidate.source_id.clone(),
                    candidate.lifecycle.as_str(),
                    event_kind,
                ],
            )
            .await?;
        let existing = rows
            .next()
            .await?
            .map(|row| {
                anyhow::Ok((
                    row.get::<String>(0)?,
                    from_i64(row.get(1)?, "operational interval revision")?,
                    row.get::<i64>(2)? != 0,
                ))
            })
            .transpose()?;
        drop(rows);
        if starts_interval {
            if let Some((identity, revision, true)) = existing {
                candidate.source_identity = identity;
                candidate.revision = revision;
                return Ok(());
            }
            candidate.revision = 1;
            self.connection
                .execute(
                    "INSERT INTO notification_operational_intervals (
                         source_id, lifecycle, event_kind, identity, revision,
                         active, started_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, 1, 1, ?5, ?5)
                     ON CONFLICT(source_id, lifecycle, event_kind) DO UPDATE SET
                         identity = excluded.identity,
                         revision = 1,
                         active = 1,
                         started_at_ms = excluded.started_at_ms,
                         updated_at_ms = excluded.updated_at_ms",
                    turso::params![
                        candidate.source_id.clone(),
                        candidate.lifecycle.as_str(),
                        event_kind,
                        candidate.source_identity.clone(),
                        candidate.occurred_at_ms,
                    ],
                )
                .await?;
        } else if let Some((identity, revision, true)) = existing {
            candidate.source_identity = identity;
            candidate.revision = revision.saturating_add(1);
            self.connection
                .execute(
                    "UPDATE notification_operational_intervals
                     SET revision = ?4, active = 0, updated_at_ms = ?5
                     WHERE source_id = ?1 AND lifecycle = ?2 AND event_kind = ?3",
                    turso::params![
                        candidate.source_id.clone(),
                        candidate.lifecycle.as_str(),
                        event_kind,
                        to_i64(candidate.revision, "operational interval revision")?,
                        candidate.occurred_at_ms,
                    ],
                )
                .await?;
        }
        Ok(())
    }

    pub(super) fn test_rule(
        &self,
        rule_id: &str,
        owner_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<ProcessSummary> {
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                let mut rows = self
                    .connection
                    .query(
                        "SELECT owner_id, active_json FROM notification_rules WHERE id = ?1",
                        turso::params![rule_id],
                    )
                    .await?;
                let Some(row) = rows.next().await? else {
                    return Err(RuleStoreError::NotFound.into());
                };
                let stored_owner = row.get::<String>(0)?;
                let active_json = row.get::<Option<String>>(1)?;
                drop(rows);
                if stored_owner != owner_id {
                    return Err(RuleStoreError::NotAuthorized.into());
                }
                let mut rule: Rule = serde_json::from_str(
                    &active_json
                        .ok_or_else(|| anyhow::anyhow!("notification rule is not active"))?,
                )?;
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
                    source_identity: identity.clone(),
                    lifecycle: Lifecycle::Test,
                    event_kind: rule
                        .filter
                        .event_kinds
                        .first()
                        .cloned()
                        .or_else(|| Some("test".to_owned())),
                    group_ids: Vec::new(),
                    zone: rule.filter.zones.first().cloned(),
                    confidence: rule.filter.minimum_confidence.or(Some(1.0)),
                    attachment_path: None,
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
                self.process_rule(&rule, &candidate, &mut summary).await?;
                Ok(summary)
            }
            .await;
            match result {
                Ok(summary) => {
                    self.connection.execute_batch("COMMIT").await?;
                    Ok(summary)
                }
                Err(error) => {
                    let _ = self.connection.execute_batch("ROLLBACK").await;
                    Err(error)
                }
            }
        })
    }

    async fn process_in_transaction(
        &self,
        candidate: &Candidate,
    ) -> anyhow::Result<ProcessSummary> {
        let rules = self.active_rules().await?;
        let mut summary = ProcessSummary::default();
        for rule in rules {
            match rule.matches(candidate)? {
                MatchResult::NoMatch => continue,
                MatchResult::Suppressed(reason) => {
                    summary.matched = summary.matched.saturating_add(1);
                    summary.suppressed = summary.suppressed.saturating_add(1);
                    let logical_id = candidate_logical_id(&rule, candidate);
                    self.record_history(
                        &logical_id,
                        &rule.id,
                        candidate,
                        "suppressed",
                        Some(reason.as_str()),
                        None,
                    )
                    .await?;
                }
                MatchResult::Match => {
                    summary.matched = summary.matched.saturating_add(1);
                    self.process_rule(&rule, candidate, &mut summary).await?;
                }
            }
        }
        Ok(summary)
    }

    async fn active_rules(&self) -> anyhow::Result<Vec<Rule>> {
        let mut rows = self
            .connection
            .query(
                "SELECT active_json FROM notification_rules
                 WHERE active_json IS NOT NULL
                 ORDER BY id",
                (),
            )
            .await?;
        let mut rules = Vec::new();
        while let Some(row) = rows.next().await? {
            rules.push(serde_json::from_str(&row.get::<String>(0)?)?);
        }
        Ok(rules)
    }

    async fn process_rule(
        &self,
        rule: &Rule,
        candidate: &Candidate,
        summary: &mut ProcessSummary,
    ) -> anyhow::Result<()> {
        let logical_id = candidate_logical_id(rule, candidate);
        if let Some(existing) = self.logical(&logical_id).await? {
            return self
                .process_existing(rule, candidate, &logical_id, existing, summary)
                .await;
        }

        let quiet_bypass = candidate.severity == Severity::Critical
            && rule.schedule.status_at(candidate.occurred_at_ms)?.quiet;
        let cooldown_eligible_at = self.cooldown_eligible_at(rule, candidate).await?;
        let rate_limited = self.rule_rate_limited(rule, candidate).await?;
        if quiet_bypass || cooldown_eligible_at.is_some() || rate_limited {
            let bypass_available = candidate.severity == Severity::Critical
                && rule.critical_bypass.is_some()
                && self.critical_bypass_available(rule, candidate).await?;
            if !bypass_available {
                summary.suppressed = summary.suppressed.saturating_add(1);
                let reason = if quiet_bypass {
                    "critical_bypass_limited"
                } else if cooldown_eligible_at.is_some() {
                    "cooldown"
                } else {
                    "rate_limited"
                };
                self.record_history(
                    &logical_id,
                    &rule.id,
                    candidate,
                    "suppressed",
                    Some(reason),
                    cooldown_eligible_at,
                )
                .await?;
                return Ok(());
            }
            self.consume_critical_bypass(rule, candidate).await?;
            self.record_audit(
                &rule.owner_id,
                "critical_bypass",
                logical_id.as_str(),
                candidate.occurred_at_ms,
                Some(candidate.revision),
            )
            .await?;
        }

        let attachment_path = usable_attachment(rule, candidate);
        let (title, body) = render_logical(rule, candidate);
        self.connection
            .execute(
                "INSERT INTO logical_notifications (
                     id, rule_id, owner_id, source_id, source_identity, lifecycle,
                     stage, highest_revision, created_at_ms, updated_at_ms,
                     enrichment_deadline_at_ms, title, body, deep_link,
                     attachment_path, severity
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10,
                           ?11, ?12, ?13, ?14, ?15)",
                turso::params![
                    logical_id.as_str(),
                    rule.id.clone(),
                    rule.owner_id.clone(),
                    candidate.source_id.clone(),
                    candidate.source_identity.clone(),
                    candidate.lifecycle.as_str(),
                    stage_str(candidate.stage),
                    to_i64(candidate.revision, "candidate revision")?,
                    candidate.occurred_at_ms,
                    add_millis(candidate.occurred_at_ms, rule.enrichment.deadline_ms),
                    title.clone(),
                    body.clone(),
                    candidate.deep_link.clone(),
                    attachment_path.clone(),
                    candidate.severity.as_str(),
                ],
            )
            .await?;
        self.connection
            .execute(
                "INSERT INTO notification_receipts (logical_id, principal_id)
                 VALUES (?1, ?2)",
                turso::params![logical_id.as_str(), rule.owner_id.clone()],
            )
            .await?;
        self.start_cooldowns(rule, candidate).await?;
        self.consume_rule_rate_limits(rule, candidate).await?;
        self.connection
            .execute(
                "UPDATE notification_rules SET last_match_at_ms = ?2 WHERE id = ?1",
                turso::params![rule.id.clone(), candidate.occurred_at_ms],
            )
            .await?;
        self.record_history(&logical_id, &rule.id, candidate, "created", None, None)
            .await?;
        summary.created = summary.created.saturating_add(1);
        summary.queued_attempts = summary.queued_attempts.saturating_add(
            self.enqueue_actions(
                rule,
                candidate,
                EnqueueContext {
                    logical_id: &logical_id,
                    default_title: &title,
                    default_body: &body,
                    attachment_path: attachment_path.as_deref(),
                    replacement: false,
                },
            )
            .await?,
        );
        Ok(())
    }

    async fn process_existing(
        &self,
        rule: &Rule,
        candidate: &Candidate,
        logical_id: &LogicalId,
        existing: StoredLogical,
        summary: &mut ProcessSummary,
    ) -> anyhow::Result<()> {
        if candidate.revision <= existing.highest_revision {
            summary.suppressed = summary.suppressed.saturating_add(1);
            self.record_history(
                logical_id,
                &rule.id,
                candidate,
                "collapsed",
                Some("duplicate_revision"),
                None,
            )
            .await?;
            return Ok(());
        }

        if candidate.revision > u64::from(rule.enrichment.maximum_revisions) {
            self.connection
                .execute(
                    "UPDATE logical_notifications
                     SET highest_revision = ?2, updated_at_ms = ?3 WHERE id = ?1",
                    turso::params![
                        logical_id.as_str(),
                        to_i64(candidate.revision, "candidate revision")?,
                        candidate.occurred_at_ms,
                    ],
                )
                .await?;
            summary.suppressed = summary.suppressed.saturating_add(1);
            self.record_history(
                logical_id,
                &rule.id,
                candidate,
                "collapsed",
                Some("enrichment_revision_limit"),
                None,
            )
            .await?;
            return Ok(());
        }

        let replace = candidate.stage == Stage::Recovery
            || (candidate.stage == Stage::Enriched && existing.stage == Stage::Preliminary);
        let late = candidate.stage == Stage::Enriched
            && candidate.occurred_at_ms > existing.enrichment_deadline_at_ms;
        let attempts_exhausted = candidate.stage == Stage::Enriched
            && existing.enrichment_attempts >= rule.enrichment.maximum_attempts;
        if !replace || attempts_exhausted || (late && !rule.enrichment.wake_after_deadline) {
            self.connection
                .execute(
                    "UPDATE logical_notifications
                     SET highest_revision = ?2, updated_at_ms = ?3
                     WHERE id = ?1",
                    turso::params![
                        logical_id.as_str(),
                        to_i64(candidate.revision, "candidate revision")?,
                        candidate.occurred_at_ms,
                    ],
                )
                .await?;
            summary.suppressed = summary.suppressed.saturating_add(1);
            self.record_history(
                logical_id,
                &rule.id,
                candidate,
                "collapsed",
                Some(if late {
                    "late_enrichment"
                } else if attempts_exhausted {
                    "enrichment_attempt_limit"
                } else {
                    "revision_collapsed"
                }),
                None,
            )
            .await?;
            return Ok(());
        }

        let attachment_path = usable_attachment(rule, candidate);
        let (title, body) = render_logical(rule, candidate);
        self.connection
            .execute(
                "UPDATE logical_notifications
                     SET stage = ?2, highest_revision = ?3,
                     enrichment_attempts = enrichment_attempts + ?4, updated_at_ms = ?5,
                     title = ?6, body = ?7, deep_link = ?8,
                     attachment_path = COALESCE(?9, attachment_path), severity = ?10
                 WHERE id = ?1",
                turso::params![
                    logical_id.as_str(),
                    stage_str(candidate.stage),
                    to_i64(candidate.revision, "candidate revision")?,
                    i64::from(candidate.stage == Stage::Enriched),
                    candidate.occurred_at_ms,
                    title.clone(),
                    body.clone(),
                    candidate.deep_link.clone(),
                    attachment_path.clone(),
                    candidate.severity.as_str(),
                ],
            )
            .await?;
        self.record_history(
            logical_id,
            &rule.id,
            candidate,
            "replaced",
            late.then_some("late_enrichment_configured"),
            None,
        )
        .await?;
        summary.replaced = summary.replaced.saturating_add(1);
        summary.queued_attempts = summary.queued_attempts.saturating_add(
            self.enqueue_actions(
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
            .await?,
        );
        Ok(())
    }

    async fn logical(&self, logical_id: &LogicalId) -> anyhow::Result<Option<StoredLogical>> {
        let mut rows = self
            .connection
            .query(
                "SELECT stage, highest_revision, enrichment_attempts,
                    enrichment_deadline_at_ms
                 FROM logical_notifications WHERE id = ?1",
                turso::params![logical_id.as_str()],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| {
                Ok(StoredLogical {
                    stage: parse_stage(&row.get::<String>(0)?)?,
                    highest_revision: from_i64(row.get(1)?, "logical revision")?,
                    enrichment_attempts: u32::try_from(row.get::<i64>(2)?)?,
                    enrichment_deadline_at_ms: row.get(3)?,
                })
            })
            .transpose()
    }

    async fn cooldown_eligible_at(
        &self,
        rule: &Rule,
        candidate: &Candidate,
    ) -> anyhow::Result<Option<i64>> {
        let keys = cooldown_keys(&policy(rule), &transition(candidate));
        let mut next = None;
        for (key, _) in keys {
            let mut rows = self
                .connection
                .query(
                    "SELECT eligible_at_ms FROM notification_cooldowns
                     WHERE rule_id = ?1 AND scope = ?2 AND scope_value = ?3",
                    turso::params![rule.id.clone(), key.scope.as_str(), key.value],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                let eligible_at_ms = row.get::<i64>(0)?;
                if candidate.occurred_at_ms < eligible_at_ms {
                    next =
                        Some(next.map_or(eligible_at_ms, |value: i64| value.max(eligible_at_ms)));
                }
            }
        }
        Ok(next)
    }

    async fn start_cooldowns(&self, rule: &Rule, candidate: &Candidate) -> anyhow::Result<()> {
        for (key, duration_ms) in cooldown_keys(&policy(rule), &transition(candidate)) {
            self.connection
                .execute(
                    "INSERT INTO notification_cooldowns (
                         rule_id, scope, scope_value, eligible_at_ms
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(rule_id, scope, scope_value) DO UPDATE SET
                         eligible_at_ms = excluded.eligible_at_ms",
                    turso::params![
                        rule.id.clone(),
                        key.scope.as_str(),
                        key.value,
                        add_millis(candidate.occurred_at_ms, duration_ms),
                    ],
                )
                .await?;
        }
        Ok(())
    }

    async fn rule_rate_limited(&self, rule: &Rule, candidate: &Candidate) -> anyhow::Result<bool> {
        for limit in &rule.rate_limits {
            if limit.scope != RateLimitScope::Channel
                && self.rate_limited(limit, rule, candidate, None).await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn consume_rule_rate_limits(
        &self,
        rule: &Rule,
        candidate: &Candidate,
    ) -> anyhow::Result<()> {
        for limit in &rule.rate_limits {
            if limit.scope != RateLimitScope::Channel {
                self.consume_rate(limit, rule, candidate, None).await?;
            }
        }
        Ok(())
    }

    async fn rate_limited(
        &self,
        limit: &RateLimit,
        rule: &Rule,
        candidate: &Candidate,
        channel: Option<Channel>,
    ) -> anyhow::Result<bool> {
        let (scope, value) = rate_key(limit.scope, rule, candidate, channel);
        self.rate_limited_for_key(limit, &scope, &value, candidate.occurred_at_ms)
            .await
    }

    async fn consume_rate(
        &self,
        limit: &RateLimit,
        rule: &Rule,
        candidate: &Candidate,
        channel: Option<Channel>,
    ) -> anyhow::Result<()> {
        let (scope, value) = rate_key(limit.scope, rule, candidate, channel);
        self.consume_rate_for_key(limit.window_ms, &scope, &value, candidate.occurred_at_ms)
            .await
    }

    async fn critical_bypass_available(
        &self,
        rule: &Rule,
        candidate: &Candidate,
    ) -> anyhow::Result<bool> {
        let bypass = rule
            .critical_bypass
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("critical bypass policy disappeared"))?;
        let limit = RateLimit {
            scope: RateLimitScope::Rule,
            maximum: bypass.maximum,
            window_ms: bypass.window_ms,
        };
        Ok(!self
            .rate_limited_for_key(
                &limit,
                "critical_bypass",
                &rule.id,
                candidate.occurred_at_ms,
            )
            .await?)
    }

    async fn consume_critical_bypass(
        &self,
        rule: &Rule,
        candidate: &Candidate,
    ) -> anyhow::Result<()> {
        let bypass = rule
            .critical_bypass
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("critical bypass policy disappeared"))?;
        self.consume_rate_for_key(
            bypass.window_ms,
            "critical_bypass",
            &rule.id,
            candidate.occurred_at_ms,
        )
        .await
    }

    async fn rate_limited_for_key(
        &self,
        limit: &RateLimit,
        scope: &str,
        value: &str,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let mut rows = self
            .connection
            .query(
                "SELECT window_started_at_ms, delivery_count
                 FROM notification_rate_windows WHERE scope = ?1 AND scope_value = ?2",
                turso::params![scope, value],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        Ok(now_ms < add_millis(row.get(0)?, limit.window_ms)
            && from_i64(row.get(1)?, "rate delivery count")? >= u64::from(limit.maximum))
    }

    async fn consume_rate_for_key(
        &self,
        window_ms: u64,
        scope: &str,
        value: &str,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let mut rows = self
            .connection
            .query(
                "SELECT window_started_at_ms, delivery_count
                 FROM notification_rate_windows WHERE scope = ?1 AND scope_value = ?2",
                turso::params![scope, value],
            )
            .await?;
        let (started_at_ms, count) = if let Some(row) = rows.next().await? {
            let started = row.get::<i64>(0)?;
            if now_ms >= add_millis(started, window_ms) {
                (now_ms, 1)
            } else {
                (started, row.get::<i64>(1)?.saturating_add(1))
            }
        } else {
            (now_ms, 1)
        };
        drop(rows);
        self.connection
            .execute(
                "INSERT INTO notification_rate_windows (
                     scope, scope_value, window_started_at_ms, delivery_count
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(scope, scope_value) DO UPDATE SET
                     window_started_at_ms = excluded.window_started_at_ms,
                     delivery_count = excluded.delivery_count",
                turso::params![scope, value, started_at_ms, count],
            )
            .await?;
        Ok(())
    }

    async fn enqueue_actions(
        &self,
        rule: &Rule,
        candidate: &Candidate,
        context: EnqueueContext<'_>,
    ) -> anyhow::Result<u32> {
        let mut queued = 0_u32;
        for (index, action) in rule.actions.iter().enumerate() {
            if context.replacement
                && !supports_replacement(action.channel)
                && !action.allow_second_delivery
            {
                self.record_history(
                    context.logical_id,
                    &rule.id,
                    candidate,
                    "collapsed",
                    Some("replacement_unsupported"),
                    None,
                )
                .await?;
                continue;
            }
            if action.attachment == AttachmentPolicy::Required && context.attachment_path.is_none()
            {
                self.record_history(
                    context.logical_id,
                    &rule.id,
                    candidate,
                    "failed",
                    Some("attachment_required"),
                    None,
                )
                .await?;
                continue;
            }
            if self
                .channel_rate_limited(rule, candidate, action.channel)
                .await?
            {
                self.record_history(
                    context.logical_id,
                    &rule.id,
                    candidate,
                    "rate_limited",
                    Some(action.channel.as_str()),
                    None,
                )
                .await?;
                continue;
            }
            if self.pending_outbox_count().await? >= MAX_PENDING_OUTBOX {
                self.record_history(
                    context.logical_id,
                    &rule.id,
                    candidate,
                    "expired",
                    Some("outbox_full"),
                    None,
                )
                .await?;
                continue;
            }
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
            let destination = serde_json::to_string(&Destination {
                value: &action.destination,
            })?;
            let payload = serde_json::to_string(&Payload {
                title,
                body,
                deep_link: &candidate.deep_link,
            })?;
            let priority = if candidate.severity == Severity::Critical {
                100
            } else {
                0
            };
            self.connection
                .execute(
                    "INSERT INTO notification_outbox (
                         logical_id, action_index, stage, channel, destination_json,
                         payload_json, replacement_key, priority, status, attempt_count,
                         max_attempts, max_retry_interval_ms, attachment_enabled,
                         attachment_required, max_attachment_bytes, next_attempt_at_ms,
                         expires_at_ms, created_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?1, ?7, 'pending', 0,
                             ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?13, ?13)
                     ON CONFLICT(logical_id, action_index, stage) DO NOTHING",
                    turso::params![
                        context.logical_id.as_str(),
                        i64::try_from(index)?,
                        stage_str(candidate.stage),
                        action.channel.as_str(),
                        destination,
                        payload,
                        priority,
                        i64::from(rule.failure.maximum_attempts),
                        to_i64(
                            rule.failure.maximum_retry_interval_ms,
                            "maximum retry interval",
                        )?,
                        i64::from(action.attachment != AttachmentPolicy::Never),
                        i64::from(action.attachment == AttachmentPolicy::Required),
                        to_i64(
                            rule.enrichment.maximum_attachment_bytes,
                            "maximum attachment bytes",
                        )?,
                        candidate.occurred_at_ms,
                        add_millis(candidate.occurred_at_ms, rule.failure.expiry_ms),
                    ],
                )
                .await?;
            self.consume_channel_rate_limits(rule, candidate, action.channel)
                .await?;
            queued = queued.saturating_add(1);
        }
        Ok(queued)
    }

    async fn channel_rate_limited(
        &self,
        rule: &Rule,
        candidate: &Candidate,
        channel: Channel,
    ) -> anyhow::Result<bool> {
        for limit in &rule.rate_limits {
            if limit.scope == RateLimitScope::Channel
                && self
                    .rate_limited(limit, rule, candidate, Some(channel))
                    .await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn consume_channel_rate_limits(
        &self,
        rule: &Rule,
        candidate: &Candidate,
        channel: Channel,
    ) -> anyhow::Result<()> {
        for limit in &rule.rate_limits {
            if limit.scope == RateLimitScope::Channel {
                self.consume_rate(limit, rule, candidate, Some(channel))
                    .await?;
            }
        }
        Ok(())
    }

    async fn pending_outbox_count(&self) -> anyhow::Result<u64> {
        let mut rows = self
            .connection
            .query(
                "SELECT COUNT(*) FROM notification_outbox
                 WHERE status IN ('pending', 'retrying')",
                (),
            )
            .await?;
        let count = rows
            .next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("outbox count query returned no row"))?
            .get::<i64>(0)?;
        from_i64(count, "pending outbox count")
    }

    async fn record_history(
        &self,
        logical_id: &LogicalId,
        rule_id: &str,
        candidate: &Candidate,
        outcome: &str,
        reason: Option<&str>,
        next_eligible_at_ms: Option<i64>,
    ) -> anyhow::Result<()> {
        self.connection
            .execute(
                "INSERT INTO notification_history (
                     logical_id, rule_id, transition_revision, stage, outcome,
                     reason, occurred_at_ms, next_eligible_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                turso::params![
                    logical_id.as_str(),
                    rule_id,
                    to_i64(candidate.revision, "history revision")?,
                    stage_str(candidate.stage),
                    outcome,
                    reason,
                    candidate.occurred_at_ms,
                    next_eligible_at_ms,
                ],
            )
            .await?;
        Ok(())
    }

    async fn record_audit(
        &self,
        principal_id: &str,
        action: &str,
        subject_id: &str,
        occurred_at_ms: i64,
        revision: Option<u64>,
    ) -> anyhow::Result<()> {
        self.connection
            .execute(
                "INSERT INTO notification_audit (
                     principal_id, action, subject_id, revision, occurred_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                turso::params![
                    principal_id,
                    action,
                    subject_id,
                    revision
                        .map(|value| to_i64(value, "audit revision"))
                        .transpose()?,
                    occurred_at_ms,
                ],
            )
            .await?;
        Ok(())
    }
}

fn candidate_logical_id(rule: &Rule, candidate: &Candidate) -> LogicalId {
    logical_id(&policy(rule), &transition(candidate))
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

fn usable_attachment(rule: &Rule, candidate: &Candidate) -> Option<String> {
    if candidate.privacy_active {
        return None;
    }
    let path = candidate.attachment_path.as_ref()?;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.len() <= rule.enrichment.maximum_attachment_bytes)
        .then(|| path.clone())
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

fn to_i64(value: u64, name: &str) -> anyhow::Result<i64> {
    i64::try_from(value).map_err(|_| anyhow::anyhow!("{name} exceeds signed 64-bit range"))
}

fn from_i64(value: i64, name: &str) -> anyhow::Result<u64> {
    u64::try_from(value).map_err(|_| anyhow::anyhow!("stored {name} is negative"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::notifications::{
        Cooldown, CooldownScope,
        model::{
            Action, CriticalBypass, EnrichmentPolicy, FailurePolicy, Filter, QuietHours, Schedule,
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
            source_identity: identity.to_owned(),
            lifecycle: if trigger == Trigger::Test {
                Lifecycle::Test
            } else {
                Lifecycle::Event
            },
            event_kind: Some("person".to_owned()),
            group_ids: Vec::new(),
            zone: None,
            confidence: Some(0.9),
            attachment_path: None,
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

    fn count(store: &Store, sql: &str) -> u64 {
        pollster::block_on(async {
            let mut rows = store.connection.query(sql, ()).await.unwrap();
            let value = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
            u64::try_from(value).unwrap()
        })
    }

    fn text(store: &Store, sql: &str) -> String {
        pollster::block_on(async {
            let mut rows = store.connection.query(sql, ()).await.unwrap();
            rows.next().await.unwrap().unwrap().get(0).unwrap()
        })
    }

    #[test]
    fn stages_and_retries_share_identity_without_cross_camera_collapse() {
        let directory = test_dir("notification-orchestration");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
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

        let enriched = candidate(
            Trigger::EventUpdated,
            "front-door",
            "event-1",
            2,
            Stage::Enriched,
            5_000,
        );
        let enriched_summary = store.process(enriched).unwrap();
        assert_eq!(enriched_summary.replaced, 1);
        assert_eq!(enriched_summary.queued_attempts, 1);

        let unrelated = candidate(
            Trigger::EventCreated,
            "back-door",
            "event-1",
            1,
            Stage::Preliminary,
            2_000,
        );
        assert_eq!(store.process(unrelated).unwrap().created, 1);
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM logical_notifications"),
            2
        );
        assert_eq!(count(&store, "SELECT COUNT(*) FROM notification_outbox"), 5);
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM notification_history WHERE reason = 'replacement_unsupported'"
            ),
            1
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cooldown_survives_restart_and_opens_at_the_exact_boundary() {
        let directory = test_dir("notification-cooldown-restart");
        let database_path = directory.join("notifications.db");
        let store = Store::open(&database_path).unwrap();
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

        let reopened = Store::open(&database_path).unwrap();
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
                .suppressed,
            1
        );
        assert_eq!(
            reopened
                .process(candidate(
                    Trigger::EventCreated,
                    "front-door",
                    "event-3",
                    1,
                    Stage::Preliminary,
                    31_000,
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
        let store = Store::open(&directory.join("notifications.db")).unwrap();
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
        let store = Store::open(&directory.join("notifications.db")).unwrap();
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
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM logical_notifications"),
            1
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn late_enrichment_and_required_attachment_are_visible() {
        let directory = test_dir("notification-late-attachment");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
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
            count(
                &store,
                "SELECT COUNT(*) FROM notification_history
                 WHERE reason IN ('attachment_required', 'late_enrichment')"
            ),
            2
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn critical_bypass_is_bounded_and_audited() {
        let directory = test_dir("notification-critical-bypass");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
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
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM notification_audit WHERE action = 'critical_bypass'"
            ),
            1
        );
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM notification_history
                 WHERE reason = 'critical_bypass_limited'"
            ),
            1
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn attachment_outbox_defers_bytes_without_disclosing_local_path() {
        let directory = test_dir("notification-attachment-payload");
        let image_path = directory.join("event-1.jpg");
        std::fs::write(&image_path, [1_u8, 2, 3, 4]).unwrap();
        let store = Store::open(&directory.join("notifications.db")).unwrap();
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
        let payload = text(&store, "SELECT payload_json FROM notification_outbox");
        assert!(!payload.contains("AQIDBA=="));
        assert!(!payload.contains(image_path.to_string_lossy().as_ref()));
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM notification_outbox WHERE attachment_enabled = 1"
            ),
            1
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
