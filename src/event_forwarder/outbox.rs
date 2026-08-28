use super::model::Publication;
use std::{fmt, path::Path, time::Duration};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_DELIVERY_RECEIPTS: i64 = 100_000;

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

pub(super) struct Outbox {
    connection: turso::Connection,
}

impl Outbox {
    pub(super) fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("MQTT outbox path is not valid UTF-8"))?;
        let database = pollster::block_on(turso::Builder::new_local(path).build())?;
        let connection = database.connect()?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        pollster::block_on(initialize_schema(&connection))?;
        Ok(Self { connection })
    }

    pub(super) fn enqueue(
        &self,
        publication: &Publication,
        limit_bytes: u64,
        now_ms: i64,
    ) -> anyhow::Result<EnqueueResult> {
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                if contains_key(&self.connection, &publication.dedup_key).await? {
                    return Ok(EnqueueResult::Duplicate);
                }
                let stats = stats(&self.connection).await?;
                let item_bytes = publication_size(publication)?;
                let attempted_bytes = stats.pending_bytes.saturating_add(item_bytes);
                if attempted_bytes > limit_bytes {
                    return Err(OutboxFull {
                        limit_bytes,
                        attempted_bytes,
                    }
                    .into());
                }
                self.connection
                    .execute(
                        "INSERT INTO mqtt_outbox (
                             dedup_key, topic, payload, qos, retain, event_timestamp_ms,
                             content_type, payload_format_indicator, correlation_data,
                             created_at_ms, attempts
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0)",
                        turso::params![
                            publication.dedup_key.clone(),
                            publication.topic.clone(),
                            publication.payload.clone(),
                            i64::from(publication.qos),
                            i64::from(publication.retain),
                            publication.event_timestamp_ms,
                            publication.content_type.clone(),
                            publication.payload_format_indicator.map(i64::from),
                            publication.correlation_data.clone(),
                            now_ms,
                        ],
                    )
                    .await?;
                Ok(EnqueueResult::Inserted)
            }
            .await;
            finish_transaction(&self.connection, result).await
        })
    }

    pub(super) fn next(&self) -> anyhow::Result<Option<OutboxItem>> {
        pollster::block_on(async {
            let mut rows = self
                .connection
                .query(
                    "SELECT sequence, dedup_key, topic, payload, qos, retain,
                            event_timestamp_ms, attempts, content_type,
                            payload_format_indicator, correlation_data
                     FROM mqtt_outbox
                     ORDER BY sequence
                     LIMIT 1",
                    (),
                )
                .await?;
            rows.next().await?.map(outbox_item).transpose()
        })
    }

    pub(super) fn mark_attempt(&self, sequence: i64, error: &str) -> anyhow::Result<()> {
        pollster::block_on(self.connection.execute(
            "UPDATE mqtt_outbox
             SET attempts = attempts + 1, last_error = ?2
             WHERE sequence = ?1",
            turso::params![sequence, error],
        ))?;
        Ok(())
    }

    pub(super) fn mark_delivered(&self, sequence: i64, delivered_at_ms: i64) -> anyhow::Result<()> {
        pollster::block_on(async {
            self.connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                self.connection
                    .execute(
                        "INSERT OR IGNORE INTO mqtt_delivery_receipts (dedup_key, delivered_at_ms)
                         SELECT dedup_key, ?2 FROM mqtt_outbox WHERE sequence = ?1",
                        turso::params![sequence, delivered_at_ms],
                    )
                    .await?;
                self.connection
                    .execute(
                        "DELETE FROM mqtt_outbox WHERE sequence = ?1",
                        turso::params![sequence],
                    )
                    .await?;
                self.connection
                    .execute(
                        "DELETE FROM mqtt_delivery_receipts
                         WHERE rowid IN (
                             SELECT rowid FROM mqtt_delivery_receipts
                             ORDER BY delivered_at_ms DESC, rowid DESC
                             LIMIT -1 OFFSET ?1
                         )",
                        [MAX_DELIVERY_RECEIPTS],
                    )
                    .await?;
                Ok(())
            }
            .await;
            finish_transaction(&self.connection, result).await
        })
    }

    pub(super) fn stats(&self) -> anyhow::Result<OutboxStats> {
        pollster::block_on(stats(&self.connection))
    }
}

async fn initialize_schema(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS mqtt_outbox (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 dedup_key TEXT NOT NULL UNIQUE,
                 topic TEXT NOT NULL,
                 payload BLOB NOT NULL,
                 qos INTEGER NOT NULL,
                 retain INTEGER NOT NULL,
                 event_timestamp_ms INTEGER NOT NULL,
                 content_type TEXT NOT NULL DEFAULT 'application/json',
                 payload_format_indicator INTEGER,
                 correlation_data BLOB NOT NULL DEFAULT X'',
                 created_at_ms INTEGER NOT NULL,
                 attempts INTEGER NOT NULL,
                 last_error TEXT
             );
             CREATE INDEX IF NOT EXISTS mqtt_outbox_event_time
                 ON mqtt_outbox(event_timestamp_ms, sequence);
             CREATE TABLE IF NOT EXISTS mqtt_delivery_receipts (
                 dedup_key TEXT PRIMARY KEY,
                 delivered_at_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS mqtt_delivery_receipts_time
                 ON mqtt_delivery_receipts(delivered_at_ms);",
        )
        .await?;
    ensure_column(
        connection,
        "content_type",
        "ALTER TABLE mqtt_outbox ADD COLUMN content_type TEXT NOT NULL DEFAULT 'application/json'",
    )
    .await?;
    ensure_column(
        connection,
        "payload_format_indicator",
        "ALTER TABLE mqtt_outbox ADD COLUMN payload_format_indicator INTEGER",
    )
    .await?;
    ensure_column(
        connection,
        "correlation_data",
        "ALTER TABLE mqtt_outbox ADD COLUMN correlation_data BLOB NOT NULL DEFAULT X''",
    )
    .await?;
    Ok(())
}

async fn ensure_column(
    connection: &turso::Connection,
    column: &str,
    statement: &str,
) -> anyhow::Result<()> {
    let mut rows = connection
        .query("PRAGMA table_info(mqtt_outbox)", ())
        .await?;
    while let Some(row) = rows.next().await? {
        if row.get::<String>(1)? == column {
            return Ok(());
        }
    }
    connection.execute(statement, ()).await?;
    Ok(())
}

async fn contains_key(connection: &turso::Connection, dedup_key: &str) -> anyhow::Result<bool> {
    Ok(connection
        .query(
            "SELECT 1 FROM mqtt_outbox WHERE dedup_key = ?1
             UNION ALL
             SELECT 1 FROM mqtt_delivery_receipts WHERE dedup_key = ?1
             LIMIT 1",
            [dedup_key],
        )
        .await?
        .next()
        .await?
        .is_some())
}

async fn stats(connection: &turso::Connection) -> anyhow::Result<OutboxStats> {
    let mut rows = connection
        .query(
            "SELECT COUNT(*),
                    COALESCE(SUM(length(dedup_key) + length(topic) + length(payload)), 0),
                    MIN(event_timestamp_ms)
             FROM mqtt_outbox",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("MQTT outbox statistics query returned no row"))?;
    Ok(OutboxStats {
        pending_items: to_u64(row.get::<i64>(0)?, "MQTT pending item count")?,
        pending_bytes: to_u64(row.get::<i64>(1)?, "MQTT pending byte count")?,
        oldest_event_timestamp_ms: row.get(2)?,
    })
}

fn outbox_item(row: turso::Row) -> anyhow::Result<OutboxItem> {
    let qos = u8::try_from(row.get::<i64>(4)?)?;
    Ok(OutboxItem {
        sequence: row.get(0)?,
        publication: Publication {
            dedup_key: row.get(1)?,
            topic: row.get(2)?,
            payload: row.get(3)?,
            qos,
            retain: row.get::<i64>(5)? != 0,
            event_timestamp_ms: row.get(6)?,
            content_type: row.get(8)?,
            payload_format_indicator: row.get::<Option<i64>>(9)?.map(u8::try_from).transpose()?,
            correlation_data: row.get(10)?,
        },
        attempts: to_u64(row.get::<i64>(7)?, "MQTT delivery attempt count")?,
    })
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

fn to_u64(value: i64, label: &str) -> anyhow::Result<u64> {
    u64::try_from(value).map_err(|_| anyhow::anyhow!("{label} is negative or too large"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("keeppeek-{name}-{}.db", uuid::Uuid::new_v4()))
    }

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
    fn persists_and_deduplicates_event_revisions() {
        let path = test_path("mqtt-outbox-dedup");
        {
            let outbox = Outbox::open(&path).unwrap();
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
        }
        let reopened = Outbox::open(&path).unwrap();
        let first = reopened.next().unwrap().unwrap();
        assert_eq!(first.publication.dedup_key, "event:home-nvr:motion-42:1");
        assert_eq!(reopened.stats().unwrap().pending_items, 2);
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_insert_before_exceeding_disk_budget() {
        let path = test_path("mqtt-outbox-bounded");
        let outbox = Outbox::open(&path).unwrap();
        let error = outbox
            .enqueue(&publication(1), 1, 1_786_800_000_001)
            .unwrap_err();
        assert!(error.downcast_ref::<OutboxFull>().is_some());
        assert_eq!(outbox.stats().unwrap().pending_items, 0);
        drop(outbox);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failed_delivery_keeps_identity_until_acknowledged() {
        let path = test_path("mqtt-outbox-redelivery");
        let outbox = Outbox::open(&path).unwrap();
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
        drop(outbox);
        let _ = std::fs::remove_file(path);
    }
}
