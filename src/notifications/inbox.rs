use super::{RuleStoreError, Stage, model::Severity, store::Store};

const MAX_PAGE_ITEMS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationItem {
    pub logical_id: String,
    pub rule_id: String,
    pub source_id: String,
    pub lifecycle: String,
    pub stage: Stage,
    pub revision: u64,
    pub title: String,
    pub body: String,
    pub deep_link: String,
    pub attachment_available: bool,
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

impl Store {
    pub(super) fn inbox(&self, principal_id: &str, limit: usize) -> anyhow::Result<Inbox> {
        pollster::block_on(async {
            let items = self.notification_items(principal_id, limit, false).await?;
            let mut rows = self
                .connection
                .query(
                    "SELECT COUNT(*)
                     FROM logical_notifications AS l
                     JOIN notification_receipts AS r ON r.logical_id = l.id
                     WHERE l.owner_id = ?1 AND r.principal_id = ?1
                       AND r.seen_at_ms IS NULL AND r.cleared_at_ms IS NULL",
                    turso::params![principal_id],
                )
                .await?;
            let unread_count = rows
                .next()
                .await?
                .ok_or_else(|| anyhow::anyhow!("notification unread query returned no row"))?
                .get::<i64>(0)?;
            Ok(Inbox {
                items,
                unread_count: from_i64(unread_count, "unread count")?,
            })
        })
    }

    pub(super) fn history(
        &self,
        principal_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<HistoryGroup>> {
        pollster::block_on(async {
            let items = self.notification_items(principal_id, limit, true).await?;
            let mut groups = Vec::with_capacity(items.len());
            for notification in items {
                let events = self.history_events(&notification.logical_id).await?;
                let attempts = self.delivery_attempts(&notification.logical_id).await?;
                groups.push(HistoryGroup {
                    notification,
                    events,
                    attempts,
                });
            }
            Ok(groups)
        })
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
            "seen_at_ms",
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
            "acknowledged_at_ms",
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
            "cleared_at_ms",
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
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                let (changed, detail) = match scope {
                    ClearScope::All => (
                        self.connection
                            .execute(
                                "UPDATE notification_receipts
                                 SET cleared_at_ms = ?2
                                 WHERE principal_id = ?1 AND cleared_at_ms IS NULL",
                                turso::params![principal_id, now_ms],
                            )
                            .await?,
                        "all".to_owned(),
                    ),
                    ClearScope::Rule(rule_id) => (
                        self.connection
                            .execute(
                                "UPDATE notification_receipts
                                 SET cleared_at_ms = ?3
                                 WHERE principal_id = ?1 AND cleared_at_ms IS NULL
                                   AND logical_id IN (
                                       SELECT id FROM logical_notifications
                                       WHERE owner_id = ?1 AND rule_id = ?2
                                   )",
                                turso::params![principal_id, rule_id.clone(), now_ms],
                            )
                            .await?,
                        format!("rule:{rule_id}"),
                    ),
                    ClearScope::Before(before_ms) => (
                        self.connection
                            .execute(
                                "UPDATE notification_receipts
                                 SET cleared_at_ms = ?3
                                 WHERE principal_id = ?1 AND cleared_at_ms IS NULL
                                   AND logical_id IN (
                                       SELECT id FROM logical_notifications
                                       WHERE owner_id = ?1 AND updated_at_ms < ?2
                                   )",
                                turso::params![principal_id, before_ms, now_ms],
                            )
                            .await?,
                        format!("before:{before_ms}"),
                    ),
                };
                self.record_inbox_audit(
                    principal_id,
                    "notifications_cleared",
                    "notifications",
                    Some(&detail),
                    now_ms,
                )
                .await?;
                Ok(changed)
            }
            .await;
            finish_transaction(&self.connection, result).await
        })
    }

    async fn notification_items(
        &self,
        principal_id: &str,
        limit: usize,
        include_cleared: bool,
    ) -> anyhow::Result<Vec<NotificationItem>> {
        let limit = limit.clamp(1, MAX_PAGE_ITEMS);
        let cleared = if include_cleared {
            ""
        } else {
            "AND r.cleared_at_ms IS NULL"
        };
        let mut rows = self
            .connection
            .query(
                format!(
                    "SELECT l.id, l.rule_id, l.source_id, l.lifecycle, l.stage,
                            l.highest_revision, l.title, l.body, l.deep_link,
                            l.attachment_path IS NOT NULL, l.severity,
                            l.created_at_ms, l.updated_at_ms,
                            r.seen_at_ms, r.acknowledged_at_ms
                     FROM logical_notifications AS l
                     JOIN notification_receipts AS r ON r.logical_id = l.id
                     WHERE l.owner_id = ?1 AND r.principal_id = ?1 {cleared}
                     ORDER BY l.updated_at_ms DESC, l.id
                     LIMIT ?2"
                ),
                turso::params![principal_id, i64::try_from(limit)?],
            )
            .await?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().await? {
            items.push(NotificationItem {
                logical_id: row.get(0)?,
                rule_id: row.get(1)?,
                source_id: row.get(2)?,
                lifecycle: row.get(3)?,
                stage: parse_stage(&row.get::<String>(4)?)?,
                revision: from_i64(row.get(5)?, "notification revision")?,
                title: row.get(6)?,
                body: row.get(7)?,
                deep_link: row.get(8)?,
                attachment_available: row.get::<i64>(9)? != 0,
                severity: parse_severity(&row.get::<String>(10)?)?,
                created_at_ms: row.get(11)?,
                updated_at_ms: row.get(12)?,
                seen_at_ms: row.get(13)?,
                acknowledged_at_ms: row.get(14)?,
            });
        }
        Ok(items)
    }

    async fn history_events(&self, logical_id: &str) -> anyhow::Result<Vec<HistoryEvent>> {
        let mut rows = self
            .connection
            .query(
                "SELECT sequence, transition_revision, stage, outcome, reason,
                        occurred_at_ms, next_eligible_at_ms
                 FROM notification_history WHERE logical_id = ?1 ORDER BY sequence",
                turso::params![logical_id],
            )
            .await?;
        let mut events = Vec::new();
        while let Some(row) = rows.next().await? {
            events.push(HistoryEvent {
                sequence: from_i64(row.get(0)?, "history sequence")?,
                revision: from_i64(row.get(1)?, "history revision")?,
                stage: parse_stage(&row.get::<String>(2)?)?,
                outcome: row.get(3)?,
                reason: row.get(4)?,
                occurred_at_ms: row.get(5)?,
                next_eligible_at_ms: row.get(6)?,
            });
        }
        Ok(events)
    }

    async fn delivery_attempts(&self, logical_id: &str) -> anyhow::Result<Vec<AttemptRecord>> {
        let mut rows = self
            .connection
            .query(
                "SELECT a.sequence, a.channel, a.stage, a.attempt, a.outcome, a.target_hash,
                        a.provider_status, a.reason, a.attempted_at_ms, a.retry_at_ms,
                        a.provider_request_id,
                        CASE WHEN a.outcome = 'delivered' THEN o.provider_acknowledged_at_ms END,
                        CASE WHEN a.outcome = 'delivered' THEN o.provider_expired_at_ms END,
                        CASE WHEN a.outcome = 'delivered' THEN o.provider_acknowledged_by_hash END,
                        CASE
                            WHEN a.outcome != 'delivered' OR o.provider_receipt IS NULL THEN NULL
                            WHEN o.provider_acknowledged_at_ms IS NOT NULL THEN 'acknowledged'
                            WHEN o.provider_expired_at_ms IS NOT NULL THEN 'expired'
                            WHEN o.next_receipt_check_at_ms IS NULL THEN 'failed'
                            ELSE 'pending'
                        END
                 FROM notification_attempts AS a
                 LEFT JOIN notification_outbox AS o ON o.id = a.outbox_id
                 WHERE a.logical_id = ?1 ORDER BY a.sequence",
                turso::params![logical_id],
            )
            .await?;
        let mut attempts = Vec::new();
        while let Some(row) = rows.next().await? {
            attempts.push(AttemptRecord {
                sequence: from_i64(row.get(0)?, "attempt sequence")?,
                channel: row.get(1)?,
                stage: parse_stage(&row.get::<String>(2)?)?,
                attempt: u32::try_from(row.get::<i64>(3)?)?,
                outcome: row.get(4)?,
                target_hash: row.get(5)?,
                provider_status: row.get::<Option<i64>>(6)?.map(u16::try_from).transpose()?,
                reason: row.get(7)?,
                attempted_at_ms: row.get(8)?,
                retry_at_ms: row.get(9)?,
                provider_request_id: row.get(10)?,
                provider_acknowledged_at_ms: row.get(11)?,
                provider_expired_at_ms: row.get(12)?,
                provider_acknowledged_by_hash: row.get(13)?,
                provider_acknowledgement_state: row.get(14)?,
            });
        }
        Ok(attempts)
    }

    fn update_receipt(
        &self,
        logical_id: &str,
        principal_id: &str,
        column: &str,
        now_ms: i64,
        audit_action: &str,
    ) -> anyhow::Result<()> {
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                let sql = match column {
                    "seen_at_ms" => {
                        "UPDATE notification_receipts SET seen_at_ms = COALESCE(seen_at_ms, ?3)
                         WHERE logical_id = ?1 AND principal_id = ?2
                           AND EXISTS (
                               SELECT 1 FROM logical_notifications
                               WHERE id = ?1 AND owner_id = ?2
                           )"
                    }
                    "acknowledged_at_ms" => {
                        "UPDATE notification_receipts
                         SET acknowledged_at_ms = COALESCE(acknowledged_at_ms, ?3)
                         WHERE logical_id = ?1 AND principal_id = ?2
                           AND EXISTS (
                               SELECT 1 FROM logical_notifications
                               WHERE id = ?1 AND owner_id = ?2
                           )"
                    }
                    "cleared_at_ms" => {
                        "UPDATE notification_receipts SET cleared_at_ms = ?3
                         WHERE logical_id = ?1 AND principal_id = ?2
                           AND EXISTS (
                               SELECT 1 FROM logical_notifications
                               WHERE id = ?1 AND owner_id = ?2
                           )"
                    }
                    _ => anyhow::bail!("unsupported notification receipt column"),
                };
                let changed = self
                    .connection
                    .execute(sql, turso::params![logical_id, principal_id, now_ms])
                    .await?;
                if changed == 0 {
                    return Err(RuleStoreError::NotAuthorized.into());
                }
                self.record_inbox_audit(principal_id, audit_action, logical_id, None, now_ms)
                    .await
            }
            .await;
            finish_transaction(&self.connection, result).await
        })
    }

    async fn record_inbox_audit(
        &self,
        principal_id: &str,
        action: &str,
        subject_id: &str,
        detail: Option<&str>,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        self.connection
            .execute(
                "INSERT INTO notification_audit (
                     principal_id, action, subject_id, detail, occurred_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                turso::params![principal_id, action, subject_id, detail, now_ms],
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

fn parse_stage(value: &str) -> anyhow::Result<Stage> {
    match value {
        "preliminary" => Ok(Stage::Preliminary),
        "enriched" => Ok(Stage::Enriched),
        "recovery" => Ok(Stage::Recovery),
        _ => anyhow::bail!("stored notification stage is invalid"),
    }
}

fn parse_severity(value: &str) -> anyhow::Result<Severity> {
    match value {
        "info" => Ok(Severity::Info),
        "warning" => Ok(Severity::Warning),
        "critical" => Ok(Severity::Critical),
        _ => anyhow::bail!("stored notification severity is invalid"),
    }
}

fn from_i64(value: i64, name: &str) -> anyhow::Result<u64> {
    u64::try_from(value).map_err(|_| anyhow::anyhow!("stored {name} is negative"))
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

    fn seed_notification(store: &Store, logical_id: &str, rule_id: &str, updated_at_ms: i64) {
        pollster::block_on(async {
            store
                .connection
                .execute(
                    "INSERT INTO logical_notifications (
                         id, rule_id, owner_id, source_id, source_identity, lifecycle,
                         stage, highest_revision, created_at_ms, updated_at_ms,
                         enrichment_deadline_at_ms, title, body, deep_link, severity
                     ) VALUES (?1, ?2, 'owner-1', 'front-door', ?1, 'event',
                               'preliminary', 1, ?3, ?3, ?3 + 10000,
                               'Person', 'Detected', '/events/test', 'info')",
                    turso::params![logical_id, rule_id, updated_at_ms],
                )
                .await
                .unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO notification_receipts (logical_id, principal_id)
                     VALUES (?1, 'owner-1')",
                    turso::params![logical_id],
                )
                .await
                .unwrap();
            store
                .connection
                .execute(
                    "INSERT INTO notification_history (
                         logical_id, rule_id, transition_revision, stage, outcome,
                         occurred_at_ms
                     ) VALUES (?1, ?2, 1, 'preliminary', 'created', ?3)",
                    turso::params![logical_id, rule_id, updated_at_ms],
                )
                .await
                .unwrap();
        });
    }

    fn count(store: &Store, sql: &str) -> u64 {
        pollster::block_on(async {
            let mut rows = store.connection.query(sql, ()).await.unwrap();
            let value = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
            u64::try_from(value).unwrap()
        })
    }

    #[test]
    fn receipts_are_principal_scoped_and_independent() {
        let directory = test_dir("notification-inbox-receipts");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
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
        assert_eq!(
            count(
                &store,
                "SELECT COUNT(*) FROM notification_audit WHERE principal_id = 'owner-1'"
            ),
            4
        );
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn scoped_clear_only_changes_the_selected_rule() {
        let directory = test_dir("notification-inbox-scope");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
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
