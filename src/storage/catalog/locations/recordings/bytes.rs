//! Maintains exact all-file bytes for the legacy-only accounting fast path.

use anyhow::{Context, Result, ensure};
use std::time::{Duration, Instant};

pub(super) async fn initialize(connection: &turso::Connection) -> Result<()> {
    ensure!(
        connection.is_autocommit()?,
        "recording byte initialization requires autocommit"
    );
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result: Result<()> = async {
        initialize_transaction(connection).await?;
        connection.execute_batch("COMMIT").await?;
        Ok(())
    }
    .await;
    if result.is_err() && !connection.is_autocommit()? {
        connection.execute_batch("ROLLBACK").await?;
    }
    result
}

async fn initialize_transaction(connection: &turso::Connection) -> Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS recording_legacy_byte_total (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1),
        total_bytes INTEGER CHECK(total_bytes IS NULL OR (typeof(total_bytes)='integer' AND total_bytes>=0))
    );
    CREATE INDEX IF NOT EXISTS storage_active_recording_allocations ON storage_volume_allocations(kind)
        WHERE kind='recording' AND state!='cancelled';").await?;
    let mut rows = connection
        .query(
            "SELECT total_bytes FROM recording_legacy_byte_total WHERE singleton=1",
            (),
        )
        .await?;
    let available = rows
        .next()
        .await?
        .map(|row| row.get::<Option<i64>>(0))
        .transpose()?
        .flatten();
    drop(rows);
    if available.is_none() {
        let total = bootstrap(connection).await?;
        if total.is_none() {
            tracing::warn!(
                "recording byte total unavailable; legacy accounting uses ownership query"
            );
        }
        connection
            .execute(
                "INSERT INTO recording_legacy_byte_total(singleton,total_bytes) VALUES (1,?1)
            ON CONFLICT(singleton) DO UPDATE SET total_bytes=excluded.total_bytes",
                [total],
            )
            .await?;
    }
    install_triggers(connection).await
}

async fn bootstrap(connection: &turso::Connection) -> Result<Option<i64>> {
    let began = Instant::now();
    let mut total = Some(0_i64);
    let mut rows = connection
        .query("SELECT file_bytes FROM recording_files", ())
        .await?;
    for _ in 0..1_000_000 {
        let Some(row) = rows.next().await? else {
            return Ok(total);
        };
        if began.elapsed() >= Duration::from_secs(2) {
            return Ok(None);
        }
        // An unavailable global total must not reject representable named/legacy accounting.
        let bytes = row.get::<i64>(0).ok().filter(|bytes| *bytes >= 0);
        total = total
            .zip(bytes)
            .and_then(|(total, bytes)| total.checked_add(bytes));
        if total.is_none() {
            return Ok(None);
        }
    }
    Ok(if rows.next().await?.is_none() {
        total
    } else {
        None
    })
}

async fn install_triggers(connection: &turso::Connection) -> Result<()> {
    // ponytail: Maintain one exact total; keep the original ownership query when it is unavailable.
    connection.execute_batch("CREATE TRIGGER IF NOT EXISTS recording_byte_total_insert
        AFTER INSERT ON recording_files BEGIN
        UPDATE recording_legacy_byte_total SET total_bytes=CASE
            WHEN total_bytes IS NULL OR typeof(NEW.file_bytes)!='integer' OR NEW.file_bytes<0 THEN NULL
            WHEN total_bytes>9223372036854775807-NEW.file_bytes THEN NULL
            ELSE total_bytes+NEW.file_bytes END WHERE singleton=1; END;
    CREATE TRIGGER IF NOT EXISTS recording_byte_total_update AFTER UPDATE OF file_bytes ON recording_files BEGIN
        UPDATE recording_legacy_byte_total SET total_bytes=CASE
            WHEN total_bytes IS NULL OR typeof(NEW.file_bytes)!='integer' OR NEW.file_bytes<0
                OR typeof(OLD.file_bytes)!='integer' OR OLD.file_bytes<0 OR total_bytes<OLD.file_bytes THEN NULL
            WHEN total_bytes-OLD.file_bytes>9223372036854775807-NEW.file_bytes THEN NULL
            ELSE total_bytes-OLD.file_bytes+NEW.file_bytes END WHERE singleton=1; END;
    CREATE TRIGGER IF NOT EXISTS recording_byte_total_delete AFTER DELETE ON recording_files BEGIN
        UPDATE recording_legacy_byte_total SET total_bytes=CASE
            WHEN total_bytes IS NULL OR typeof(OLD.file_bytes)!='integer' OR OLD.file_bytes<0
                OR total_bytes<OLD.file_bytes THEN NULL ELSE total_bytes-OLD.file_bytes END WHERE singleton=1; END;").await?;
    Ok(())
}

pub(super) async fn read(connection: &turso::Connection) -> Result<Option<u64>> {
    let mut rows = connection
        .query(
            "SELECT total_bytes FROM recording_legacy_byte_total WHERE singleton=1",
            (),
        )
        .await?;
    let total = rows
        .next()
        .await?
        .context("recording byte total missing")?
        .get::<Option<i64>>(0)?;
    total
        .map(|bytes| super::to_u64(bytes, "recording byte total"))
        .transpose()
}
