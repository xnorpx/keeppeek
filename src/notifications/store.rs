use std::{fmt, path::Path, time::Duration};

use super::model::Rule;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RULE_JSON_BYTES: usize = 64 * 1_024;

#[derive(Debug, Clone, PartialEq)]
pub struct RuleRecord {
    pub id: String,
    pub owner_id: String,
    pub active: Option<Rule>,
    pub active_revision: u64,
    pub draft: Rule,
    pub draft_revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub last_match_at_ms: Option<i64>,
    pub last_delivery_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleStoreError {
    Conflict {
        active_revision: u64,
        draft_revision: u64,
    },
    NotFound,
    NotAuthorized,
}

impl fmt::Display for RuleStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict {
                active_revision,
                draft_revision,
            } => write!(
                formatter,
                "notification rule revision conflict (active {active_revision}, draft {draft_revision})"
            ),
            Self::NotFound => formatter.write_str("notification rule was not found"),
            Self::NotAuthorized => {
                formatter.write_str("notification rule is owned by another principal")
            }
        }
    }
}

impl std::error::Error for RuleStoreError {}

pub(super) struct Store {
    pub(super) connection: turso::Connection,
}

impl Store {
    pub(super) fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("notification database path is not valid UTF-8"))?;
        let database = pollster::block_on(turso::Builder::new_local(path).build())?;
        let connection = database.connect()?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        pollster::block_on(initialize_schema(&connection))?;
        Ok(Self { connection })
    }

    pub(super) fn save_draft(
        &self,
        mut draft: Rule,
        expected_draft_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<RuleRecord> {
        pollster::block_on(async {
            validate_draft_identity(&draft)?;
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                let existing = rule_by_id(&self.connection, &draft.id).await?;
                let next_draft_revision = match &existing {
                    Some(existing) => {
                        ensure_owner(existing, &draft.owner_id)?;
                        ensure_revisions(
                            existing,
                            existing.active_revision,
                            expected_draft_revision,
                        )?;
                        existing.draft_revision.saturating_add(1)
                    }
                    None if expected_draft_revision == 0 => 1,
                    None => {
                        return Err(RuleStoreError::Conflict {
                            active_revision: 0,
                            draft_revision: 0,
                        }
                        .into());
                    }
                };
                draft.revision = next_draft_revision;
                let draft_json = serialize_rule(&draft)?;
                self.connection
                    .execute(
                        "INSERT INTO notification_rules (
                             id, owner_id, name, active_revision, active_json, draft_revision,
                             draft_json, created_at_ms, updated_at_ms
                         ) VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?5, ?6, ?6)
                         ON CONFLICT(id) DO UPDATE SET
                             name = excluded.name,
                             draft_revision = excluded.draft_revision,
                             draft_json = excluded.draft_json,
                             updated_at_ms = excluded.updated_at_ms",
                        turso::params![
                            draft.id.clone(),
                            draft.owner_id.clone(),
                            draft.name.clone(),
                            to_i64(next_draft_revision, "draft revision")?,
                            draft_json,
                            now_ms,
                        ],
                    )
                    .await?;
                record_audit(
                    &self.connection,
                    &draft.owner_id,
                    "rule_draft_saved",
                    &draft.id,
                    now_ms,
                    None,
                )
                .await?;
                rule_by_id(&self.connection, &draft.id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("saved notification rule disappeared"))
            }
            .await;
            finish_transaction(&self.connection, result).await
        })
    }

    pub(super) fn activate(
        &self,
        rule_id: &str,
        owner_id: &str,
        expected_active_revision: u64,
        expected_draft_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<RuleRecord> {
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                let existing = rule_by_id(&self.connection, rule_id)
                    .await?
                    .ok_or(RuleStoreError::NotFound)?;
                ensure_owner(&existing, owner_id)?;
                ensure_revisions(&existing, expected_active_revision, expected_draft_revision)?;
                existing.draft.validate()?;

                let active_revision = existing.active_revision.saturating_add(1);
                let mut active = existing.draft;
                active.revision = active_revision;
                let active_json = serialize_rule(&active)?;
                self.connection
                    .execute(
                        "INSERT INTO notification_rule_versions (
                             rule_id, revision, definition_json, activated_at_ms, activated_by
                         ) VALUES (?1, ?2, ?3, ?4, ?5)",
                        turso::params![
                            rule_id,
                            to_i64(active_revision, "active revision")?,
                            active_json.clone(),
                            now_ms,
                            owner_id,
                        ],
                    )
                    .await?;
                self.connection
                    .execute(
                        "UPDATE notification_rules
                         SET active_revision = ?2, active_json = ?3, updated_at_ms = ?4
                         WHERE id = ?1",
                        turso::params![
                            rule_id,
                            to_i64(active_revision, "active revision")?,
                            active_json,
                            now_ms,
                        ],
                    )
                    .await?;
                cancel_disabled_outbox(&self.connection, rule_id, Some(&active), now_ms).await?;
                record_audit(
                    &self.connection,
                    owner_id,
                    "rule_activated",
                    rule_id,
                    now_ms,
                    Some(active_revision),
                )
                .await?;
                rule_by_id(&self.connection, rule_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("activated notification rule disappeared"))
            }
            .await;
            finish_transaction(&self.connection, result).await
        })
    }

    pub(super) fn delete(
        &self,
        rule_id: &str,
        owner_id: &str,
        expected_active_revision: u64,
        expected_draft_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                let existing = rule_by_id(&self.connection, rule_id)
                    .await?
                    .ok_or(RuleStoreError::NotFound)?;
                ensure_owner(&existing, owner_id)?;
                ensure_revisions(&existing, expected_active_revision, expected_draft_revision)?;
                cancel_disabled_outbox(&self.connection, rule_id, None, now_ms).await?;
                self.connection
                    .execute(
                        "DELETE FROM notification_rules WHERE id = ?1",
                        turso::params![rule_id],
                    )
                    .await?;
                record_audit(
                    &self.connection,
                    owner_id,
                    "rule_deleted",
                    rule_id,
                    now_ms,
                    Some(existing.active_revision),
                )
                .await
            }
            .await;
            finish_transaction(&self.connection, result).await
        })
    }

    pub(super) fn rules(&self, owner_id: &str) -> anyhow::Result<Vec<RuleRecord>> {
        pollster::block_on(async {
            let mut rows = self
                .connection
                .query(
                    "SELECT id, owner_id, active_revision, active_json, draft_revision,
                            draft_json, created_at_ms, updated_at_ms,
                            last_match_at_ms, last_delivery_at_ms
                     FROM notification_rules
                     WHERE owner_id = ?1
                     ORDER BY name COLLATE NOCASE, id",
                    turso::params![owner_id],
                )
                .await?;
            let mut records = Vec::new();
            while let Some(row) = rows.next().await? {
                records.push(rule_record(row)?);
            }
            Ok(records)
        })
    }

    #[cfg(test)]
    pub(super) fn rule(&self, rule_id: &str) -> anyhow::Result<Option<RuleRecord>> {
        pollster::block_on(rule_by_id(&self.connection, rule_id))
    }
}

async fn initialize_schema(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS notification_rules (
                 id TEXT PRIMARY KEY,
                 owner_id TEXT NOT NULL,
                 name TEXT NOT NULL,
                 active_revision INTEGER,
                 active_json TEXT,
                 draft_revision INTEGER NOT NULL,
                 draft_json TEXT NOT NULL,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL,
                 last_match_at_ms INTEGER,
                 last_delivery_at_ms INTEGER
             );
             CREATE INDEX IF NOT EXISTS notification_rules_owner_name
                 ON notification_rules(owner_id, name COLLATE NOCASE, id);
             CREATE TABLE IF NOT EXISTS notification_rule_versions (
                 rule_id TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 definition_json TEXT NOT NULL,
                 activated_at_ms INTEGER NOT NULL,
                 activated_by TEXT NOT NULL,
                 PRIMARY KEY(rule_id, revision)
             );
             CREATE TABLE IF NOT EXISTS logical_notifications (
                 id TEXT PRIMARY KEY,
                 rule_id TEXT NOT NULL,
                 owner_id TEXT NOT NULL,
                 source_id TEXT NOT NULL,
                 source_identity TEXT NOT NULL,
                 lifecycle TEXT NOT NULL,
                 stage TEXT NOT NULL,
                 highest_revision INTEGER NOT NULL,
                 enrichment_attempts INTEGER NOT NULL DEFAULT 0,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL,
                 enrichment_deadline_at_ms INTEGER NOT NULL,
                 title TEXT NOT NULL,
                 body TEXT NOT NULL,
                 deep_link TEXT NOT NULL,
                 attachment_path TEXT,
                 severity TEXT NOT NULL,
                 canonical_attachment_json TEXT,
                 icon_key TEXT,
                 image_available INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS logical_notifications_owner_time
                 ON logical_notifications(owner_id, updated_at_ms DESC, id);
             CREATE TABLE IF NOT EXISTS notification_operational_intervals (
                 source_id TEXT NOT NULL,
                 lifecycle TEXT NOT NULL,
                 event_kind TEXT NOT NULL,
                 identity TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 active INTEGER NOT NULL,
                 started_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL,
                 PRIMARY KEY(source_id, lifecycle, event_kind)
             );
             CREATE TABLE IF NOT EXISTS notification_receipts (
                 logical_id TEXT NOT NULL REFERENCES logical_notifications(id) ON DELETE CASCADE,
                 principal_id TEXT NOT NULL,
                 seen_at_ms INTEGER,
                 acknowledged_at_ms INTEGER,
                 cleared_at_ms INTEGER,
                 PRIMARY KEY(logical_id, principal_id)
             );
             CREATE INDEX IF NOT EXISTS notification_receipts_unread
                 ON notification_receipts(principal_id, seen_at_ms, cleared_at_ms);
             CREATE TABLE IF NOT EXISTS notification_history (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 logical_id TEXT NOT NULL,
                 rule_id TEXT NOT NULL,
                 transition_revision INTEGER NOT NULL,
                 stage TEXT NOT NULL,
                 outcome TEXT NOT NULL,
                 reason TEXT,
                 occurred_at_ms INTEGER NOT NULL,
                 next_eligible_at_ms INTEGER
             );
             CREATE INDEX IF NOT EXISTS notification_history_logical
                 ON notification_history(logical_id, sequence);
             CREATE TABLE IF NOT EXISTS notification_cooldowns (
                 rule_id TEXT NOT NULL,
                 scope TEXT NOT NULL,
                 scope_value TEXT NOT NULL,
                 eligible_at_ms INTEGER NOT NULL,
                 PRIMARY KEY(rule_id, scope, scope_value)
             );
             CREATE TABLE IF NOT EXISTS notification_rate_windows (
                 scope TEXT NOT NULL,
                 scope_value TEXT NOT NULL,
                 window_started_at_ms INTEGER NOT NULL,
                 delivery_count INTEGER NOT NULL,
                 PRIMARY KEY(scope, scope_value)
             );
             CREATE TABLE IF NOT EXISTS notification_outbox (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 logical_id TEXT NOT NULL,
                 action_index INTEGER NOT NULL,
                 stage TEXT NOT NULL,
                 channel TEXT NOT NULL,
                 destination_json TEXT NOT NULL,
                 payload_json TEXT NOT NULL,
                 replacement_key TEXT NOT NULL,
                 priority INTEGER NOT NULL,
                 status TEXT NOT NULL,
                 attempt_count INTEGER NOT NULL,
                 max_attempts INTEGER NOT NULL,
                 max_retry_interval_ms INTEGER NOT NULL,
                 attachment_enabled INTEGER NOT NULL DEFAULT 0,
                 attachment_required INTEGER NOT NULL DEFAULT 0,
                 max_attachment_bytes INTEGER NOT NULL DEFAULT 4194304,
                 next_attempt_at_ms INTEGER NOT NULL,
                 expires_at_ms INTEGER NOT NULL,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL,
                 last_reason TEXT,
                 provider_request_id TEXT,
                 provider_receipt TEXT,
                 next_receipt_check_at_ms INTEGER,
                 provider_receipt_expires_at_ms INTEGER,
                 provider_acknowledged_at_ms INTEGER,
                 provider_expired_at_ms INTEGER,
                 provider_acknowledged_by_hash TEXT,
                 UNIQUE(logical_id, action_index, stage)
             );
             CREATE INDEX IF NOT EXISTS notification_outbox_due
                 ON notification_outbox(status, next_attempt_at_ms, priority DESC, id);
             CREATE TABLE IF NOT EXISTS notification_attempts (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 outbox_id INTEGER NOT NULL,
                 logical_id TEXT NOT NULL,
                 channel TEXT NOT NULL,
                 stage TEXT NOT NULL,
                 attempt INTEGER NOT NULL,
                 outcome TEXT NOT NULL,
                 target_hash TEXT NOT NULL,
                 provider_status INTEGER,
                 provider_request_id TEXT,
                 reason TEXT,
                 attempted_at_ms INTEGER NOT NULL,
                 retry_at_ms INTEGER
             );
             CREATE INDEX IF NOT EXISTS notification_attempts_logical
                 ON notification_attempts(logical_id, sequence);
             CREATE TABLE IF NOT EXISTS notification_audit (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 principal_id TEXT NOT NULL,
                 action TEXT NOT NULL,
                 subject_id TEXT NOT NULL,
                 revision INTEGER,
                 detail TEXT,
                 occurred_at_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS notification_audit_time
                 ON notification_audit(occurred_at_ms DESC, sequence DESC);",
        )
        .await?;
    ensure_column(
        connection,
        "logical_notifications",
        "enrichment_attempts",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(
        connection,
        "logical_notifications",
        "canonical_attachment_json",
        "TEXT",
    )
    .await?;
    ensure_column(connection, "logical_notifications", "icon_key", "TEXT").await?;
    ensure_column(
        connection,
        "logical_notifications",
        "image_available",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "attachment_enabled",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "attachment_required",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "max_attachment_bytes",
        "INTEGER NOT NULL DEFAULT 4194304",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "max_attempts",
        "INTEGER NOT NULL DEFAULT 1",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "max_retry_interval_ms",
        "INTEGER NOT NULL DEFAULT 1000",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "provider_request_id",
        "TEXT",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "provider_receipt",
        "TEXT",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "next_receipt_check_at_ms",
        "INTEGER",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "provider_receipt_expires_at_ms",
        "INTEGER",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "provider_acknowledged_at_ms",
        "INTEGER",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "provider_expired_at_ms",
        "INTEGER",
    )
    .await?;
    ensure_column(
        connection,
        "notification_outbox",
        "provider_acknowledged_by_hash",
        "TEXT",
    )
    .await?;
    ensure_column(
        connection,
        "notification_attempts",
        "provider_request_id",
        "TEXT",
    )
    .await?;
    Ok(())
}

async fn ensure_column(
    connection: &turso::Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(format!("PRAGMA table_info({table})"), ())
        .await?;
    while let Some(row) = rows.next().await? {
        if row.get::<String>(1)? == column {
            return Ok(());
        }
    }
    connection
        .execute_batch(format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))
        .await?;
    Ok(())
}

async fn rule_by_id(
    connection: &turso::Connection,
    rule_id: &str,
) -> anyhow::Result<Option<RuleRecord>> {
    let mut rows = connection
        .query(
            "SELECT id, owner_id, active_revision, active_json, draft_revision,
                    draft_json, created_at_ms, updated_at_ms,
                    last_match_at_ms, last_delivery_at_ms
             FROM notification_rules WHERE id = ?1",
            turso::params![rule_id],
        )
        .await?;
    rows.next().await?.map(rule_record).transpose()
}

fn rule_record(row: turso::Row) -> anyhow::Result<RuleRecord> {
    let active_json = row.get::<Option<String>>(3)?;
    Ok(RuleRecord {
        id: row.get(0)?,
        owner_id: row.get(1)?,
        active: active_json
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
        active_revision: from_optional_i64(row.get(2)?, "active revision")?,
        draft_revision: from_i64(row.get(4)?, "draft revision")?,
        draft: serde_json::from_str(&row.get::<String>(5)?)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
        last_match_at_ms: row.get(8)?,
        last_delivery_at_ms: row.get(9)?,
    })
}

fn serialize_rule(rule: &Rule) -> anyhow::Result<String> {
    let serialized = serde_json::to_string(rule)?;
    if serialized.len() > MAX_RULE_JSON_BYTES {
        anyhow::bail!("notification rule exceeds {MAX_RULE_JSON_BYTES} serialized bytes");
    }
    Ok(serialized)
}

fn validate_draft_identity(rule: &Rule) -> anyhow::Result<()> {
    if rule.id.is_empty() || rule.owner_id.is_empty() {
        anyhow::bail!("notification draft requires a rule ID and owner");
    }
    serialize_rule(rule).map(|_| ())
}

fn ensure_owner(record: &RuleRecord, owner_id: &str) -> anyhow::Result<()> {
    if record.owner_id == owner_id {
        Ok(())
    } else {
        Err(RuleStoreError::NotAuthorized.into())
    }
}

fn ensure_revisions(
    record: &RuleRecord,
    expected_active_revision: u64,
    expected_draft_revision: u64,
) -> anyhow::Result<()> {
    if record.active_revision == expected_active_revision
        && record.draft_revision == expected_draft_revision
    {
        Ok(())
    } else {
        Err(RuleStoreError::Conflict {
            active_revision: record.active_revision,
            draft_revision: record.draft_revision,
        }
        .into())
    }
}

async fn record_audit(
    connection: &turso::Connection,
    principal_id: &str,
    action: &str,
    subject_id: &str,
    occurred_at_ms: i64,
    revision: Option<u64>,
) -> anyhow::Result<()> {
    connection
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

async fn cancel_disabled_outbox(
    connection: &turso::Connection,
    rule_id: &str,
    active: Option<&Rule>,
    now_ms: i64,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query(
            "SELECT o.id, o.logical_id, o.action_index, o.channel, o.stage
             FROM notification_outbox AS o
             JOIN logical_notifications AS l ON l.id = o.logical_id
             WHERE l.rule_id = ?1 AND o.status IN ('pending', 'retrying')",
            turso::params![rule_id],
        )
        .await?;
    let mut cancelled = Vec::new();
    while let Some(row) = rows.next().await? {
        let action_index = row.get::<i64>(2)?;
        let channel = row.get::<String>(3)?;
        let allowed = active.is_some_and(|rule| {
            rule.enabled
                && usize::try_from(action_index)
                    .ok()
                    .and_then(|index| rule.actions.get(index))
                    .is_some_and(|action| action.enabled && action.channel.as_str() == channel)
        });
        if !allowed {
            cancelled.push((
                row.get::<i64>(0)?,
                row.get::<String>(1)?,
                row.get::<String>(4)?,
            ));
        }
    }
    drop(rows);
    for (outbox_id, logical_id, stage) in cancelled {
        let changed = connection
            .execute(
                "UPDATE notification_outbox
                 SET status = 'expired', updated_at_ms = ?2,
                     last_reason = 'rule_or_action_disabled'
                 WHERE id = ?1 AND status IN ('pending', 'retrying')",
                turso::params![outbox_id, now_ms],
            )
            .await?;
        if changed == 0 {
            continue;
        }
        connection
            .execute(
                "INSERT INTO notification_history (
                     logical_id, rule_id, transition_revision, stage, outcome,
                     reason, occurred_at_ms
                 ) SELECT id, rule_id, highest_revision, ?3, 'expired',
                          'rule_or_action_disabled', ?4
                   FROM logical_notifications WHERE id = ?1 AND rule_id = ?2",
                turso::params![logical_id, rule_id, stage, now_ms],
            )
            .await?;
    }
    Ok(())
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

fn to_i64(value: u64, name: &str) -> anyhow::Result<i64> {
    i64::try_from(value).map_err(|_| anyhow::anyhow!("{name} exceeds signed 64-bit range"))
}

fn from_i64(value: i64, name: &str) -> anyhow::Result<u64> {
    u64::try_from(value).map_err(|_| anyhow::anyhow!("stored {name} is negative"))
}

fn from_optional_i64(value: Option<i64>, name: &str) -> anyhow::Result<u64> {
    value.map(|value| from_i64(value, name)).unwrap_or(Ok(0))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::notifications::{
        Cooldown, CooldownScope,
        model::{
            Action, AttachmentPolicy, Channel, EnrichmentPolicy, FailurePolicy, Filter, Schedule,
            Template, Trigger,
        },
    };

    fn test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("keeppeek-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn draft(id: &str) -> Rule {
        Rule {
            id: id.to_owned(),
            name: "Front door person".to_owned(),
            enabled: true,
            revision: 0,
            owner_id: "owner-1".to_owned(),
            triggers: vec![Trigger::EventCreated, Trigger::EventUpdated],
            filter: Filter::default(),
            schedule: Schedule {
                timezone: "UTC".to_owned(),
                active_windows: Vec::new(),
                quiet_hours: None,
            },
            cooldowns: vec![Cooldown {
                scope: CooldownScope::Event,
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
            actions: vec![Action {
                enabled: true,
                channel: Channel::Browser,
                destination: String::new(),
                template: Template {
                    title: "Person detected".to_owned(),
                    body: "Open {{notification.deep_link}}".to_owned(),
                },
                attachment: AttachmentPolicy::WhenAvailable,
                allow_second_delivery: false,
            }],
            failure: FailurePolicy {
                maximum_attempts: 3,
                maximum_retry_interval_ms: 60_000,
                expiry_ms: 3_600_000,
            },
        }
    }

    #[test]
    fn activation_is_atomic_and_preserves_an_invalid_draft() {
        let directory = test_dir("notification-rule-activation");
        let database_path = directory.join("notifications.db");
        let store = Store::open(&database_path).unwrap();

        let saved = store.save_draft(draft("rule-1"), 0, 1_000).unwrap();
        let active = store
            .activate("rule-1", "owner-1", 0, saved.draft_revision, 2_000)
            .unwrap();
        assert_eq!(active.active_revision, 1);

        let mut invalid = active.draft;
        invalid.actions.clear();
        let saved = store
            .save_draft(invalid, active.draft_revision, 3_000)
            .unwrap();
        assert!(
            store
                .activate(
                    "rule-1",
                    "owner-1",
                    saved.active_revision,
                    saved.draft_revision,
                    4_000,
                )
                .is_err()
        );

        drop(store);
        let reopened = Store::open(&database_path).unwrap();
        let persisted = reopened.rule("rule-1").unwrap().unwrap();
        assert_eq!(persisted.active_revision, 1);
        assert_eq!(persisted.active.unwrap().actions.len(), 1);
        assert!(persisted.draft.actions.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_draft_write_reports_both_current_revisions() {
        let directory = test_dir("notification-rule-conflict");
        let store = Store::open(&directory.join("notifications.db")).unwrap();
        let saved = store.save_draft(draft("rule-1"), 0, 1_000).unwrap();
        let active = store
            .activate("rule-1", "owner-1", 0, saved.draft_revision, 2_000)
            .unwrap();

        let error = store.save_draft(draft("rule-1"), 0, 3_000).unwrap_err();
        assert_eq!(
            error.downcast_ref::<RuleStoreError>(),
            Some(&RuleStoreError::Conflict {
                active_revision: active.active_revision,
                draft_revision: active.draft_revision,
            })
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
