//! Durable latest-row storage with tombstone fencing and bounded retention.
//!
//! No source IP addresses, client headers, location data, or activity history are stored.

use crate::retention::MAX_RETENTION_SECS;
use crate::schema::UsageReportV1;
use sha2::{Digest, Sha256};
use std::path::Path;
use subtle::ConstantTimeEq as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    Replaced,
    Idempotent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    AlreadyDeleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiryReport {
    pub expired_installations: u64,
    pub expired_tombstones: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminStats {
    pub active_installations: u64,
    pub active_tombstones: u64,
    pub expired_installations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Unauthorized,
    StaleSequence,
    Deleted,
    Storage(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("installation proof token mismatch"),
            Self::StaleSequence => formatter.write_str("report sequence is stale"),
            Self::Deleted => formatter.write_str("installation identity was deleted"),
            Self::Storage(detail) => write!(formatter, "storage error: {detail}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub struct ReceiverStore {
    connection: turso::Connection,
}

impl ReceiverStore {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("receiver database path is not valid UTF-8"))?;
        let database = pollster::block_on(turso::Builder::new_local(path_str).build())?;
        let connection = database.connect()?;
        pollster::block_on(initialize_schema(&connection))?;
        Ok(Self { connection })
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let database = pollster::block_on(turso::Builder::new_local(":memory:").build())?;
        let connection = database.connect()?;
        pollster::block_on(initialize_schema(&connection))?;
        Ok(Self { connection })
    }

    pub fn upsert_report(
        &mut self,
        report: &UsageReportV1,
        canonical_json: &str,
        token: &str,
        now_secs: i64,
    ) -> Result<UpsertOutcome, StoreError> {
        let token_hash = sha256_bytes(token.as_bytes());
        let payload_hash = sha256_bytes(canonical_json.as_bytes());
        let expires_at = now_secs.saturating_add(MAX_RETENTION_SECS);
        pollster::block_on(self.upsert_transaction(
            &report.installation_id,
            &token_hash,
            report.report_sequence,
            &payload_hash,
            canonical_json,
            now_secs,
            expires_at,
        ))
    }

    pub fn delete_installation(
        &mut self,
        installation_id: &str,
        token: &str,
        now_secs: i64,
    ) -> Result<DeleteOutcome, StoreError> {
        let token_hash = sha256_bytes(token.as_bytes());
        let expires_at = now_secs.saturating_add(MAX_RETENTION_SECS);
        pollster::block_on(self.delete_transaction(
            installation_id,
            &token_hash,
            now_secs,
            expires_at,
        ))
    }

    pub fn purge_expired(&mut self, now_secs: i64) -> Result<ExpiryReport, StoreError> {
        pollster::block_on(self.purge_expired_transaction(now_secs))
    }

    pub fn active_reports(&self, now_secs: i64) -> Result<Vec<UsageReportV1>, StoreError> {
        pollster::block_on(self.read_active_reports(now_secs))
    }

    pub fn admin_stats(&self, now_secs: i64) -> Result<AdminStats, StoreError> {
        pollster::block_on(self.read_admin_stats(now_secs))
    }

    pub fn table_columns(&self, table: &str) -> Result<Vec<String>, StoreError> {
        pollster::block_on(self.read_table_columns(table))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "transaction helper carries validated installation metadata directly"
    )]
    async fn upsert_transaction(
        &self,
        installation_id: &str,
        token_hash: &[u8; 32],
        report_sequence: u64,
        payload_hash: &[u8; 32],
        canonical_json: &str,
        now_secs: i64,
        expires_at: i64,
    ) -> Result<UpsertOutcome, StoreError> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .await
            .map_err(storage_err)?;
        let result = self
            .upsert_inner(
                installation_id,
                token_hash,
                report_sequence,
                payload_hash,
                canonical_json,
                now_secs,
                expires_at,
            )
            .await;
        self.finish_tx(result).await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "transaction helper carries validated installation metadata directly"
    )]
    async fn upsert_inner(
        &self,
        installation_id: &str,
        token_hash: &[u8; 32],
        report_sequence: u64,
        payload_hash: &[u8; 32],
        canonical_json: &str,
        now_secs: i64,
        expires_at: i64,
    ) -> Result<UpsertOutcome, StoreError> {
        let sequence = i64::try_from(report_sequence).map_err(|_| StoreError::StaleSequence)?;
        if let Some((_, tombstone_expires)) = self.find_tombstone(installation_id).await?
            && tombstone_expires > now_secs
        {
            return Err(StoreError::Deleted);
        }

        if let Some(existing) = self.find_installation(installation_id).await?
            && existing.expires_at_secs > now_secs
        {
            if !hashes_match(&existing.token_hash, token_hash) {
                return Err(StoreError::Unauthorized);
            }
            if sequence < existing.report_sequence {
                return Err(StoreError::StaleSequence);
            }
            if sequence == existing.report_sequence {
                return if hashes_match(&existing.payload_hash, payload_hash) {
                    Ok(UpsertOutcome::Idempotent)
                } else {
                    Err(StoreError::StaleSequence)
                };
            }
        }

        self.connection
            .execute(
                "INSERT OR REPLACE INTO installations \
                 (installation_id, token_hash, report_sequence, payload_hash, payload_json, updated_at_secs, expires_at_secs) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                turso::params![
                    installation_id,
                    token_hash.as_slice(),
                    sequence,
                    payload_hash.as_slice(),
                    canonical_json,
                    now_secs,
                    expires_at,
                ],
            )
            .await
            .map_err(storage_err)?;
        Ok(UpsertOutcome::Replaced)
    }

    async fn delete_transaction(
        &self,
        installation_id: &str,
        token_hash: &[u8; 32],
        now_secs: i64,
        expires_at: i64,
    ) -> Result<DeleteOutcome, StoreError> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .await
            .map_err(storage_err)?;
        let result = self
            .delete_inner(installation_id, token_hash, now_secs, expires_at)
            .await;
        self.finish_tx(result).await
    }

    async fn delete_inner(
        &self,
        installation_id: &str,
        token_hash: &[u8; 32],
        now_secs: i64,
        expires_at: i64,
    ) -> Result<DeleteOutcome, StoreError> {
        let mut had_active = false;
        if let Some(existing) = self.find_installation(installation_id).await?
            && existing.expires_at_secs > now_secs
        {
            if !hashes_match(&existing.token_hash, token_hash) {
                return Err(StoreError::Unauthorized);
            }
            had_active = true;
        }
        if let Some((stored_hash, tombstone_expires)) = self.find_tombstone(installation_id).await?
            && tombstone_expires > now_secs
            && !hashes_match(&stored_hash, token_hash)
        {
            return Err(StoreError::Unauthorized);
        }

        self.connection
            .execute(
                "DELETE FROM installations WHERE installation_id = ?1",
                turso::params![installation_id],
            )
            .await
            .map_err(storage_err)?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO tombstones \
                 (installation_id, token_hash, deleted_at_secs, expires_at_secs) \
                 VALUES (?1, ?2, ?3, ?4)",
                turso::params![installation_id, token_hash.as_slice(), now_secs, expires_at],
            )
            .await
            .map_err(storage_err)?;

        Ok(if had_active {
            DeleteOutcome::Deleted
        } else {
            DeleteOutcome::AlreadyDeleted
        })
    }

    async fn purge_expired_transaction(&self, now_secs: i64) -> Result<ExpiryReport, StoreError> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .await
            .map_err(storage_err)?;
        let result = async {
            let expired_installations = self
                .connection
                .execute(
                    "DELETE FROM installations WHERE expires_at_secs <= ?1",
                    turso::params![now_secs],
                )
                .await
                .map_err(storage_err)?;
            let expired_tombstones = self
                .connection
                .execute(
                    "DELETE FROM tombstones WHERE expires_at_secs <= ?1",
                    turso::params![now_secs],
                )
                .await
                .map_err(storage_err)?;
            Ok(ExpiryReport {
                expired_installations,
                expired_tombstones,
            })
        }
        .await;
        self.finish_tx(result).await
    }

    async fn read_active_reports(&self, now_secs: i64) -> Result<Vec<UsageReportV1>, StoreError> {
        let mut rows = self
            .connection
            .query(
                "SELECT payload_json FROM installations WHERE expires_at_secs > ?1 ORDER BY installation_id",
                turso::params![now_secs],
            )
            .await
            .map_err(storage_err)?;
        let mut reports = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage_err)? {
            let json = row.get::<String>(0).map_err(storage_err)?;
            let parsed: UsageReportV1 = serde_json::from_str(&json)
                .map_err(|error| StoreError::Storage(error.to_string()))?;
            reports.push(parsed);
        }
        Ok(reports)
    }

    async fn read_admin_stats(&self, now_secs: i64) -> Result<AdminStats, StoreError> {
        let active_installations = self
            .count_query(
                "SELECT COUNT(*) FROM installations WHERE expires_at_secs > ?1",
                now_secs,
            )
            .await?;
        let expired_installations = self
            .count_query(
                "SELECT COUNT(*) FROM installations WHERE expires_at_secs <= ?1",
                now_secs,
            )
            .await?;
        let active_tombstones = self
            .count_query(
                "SELECT COUNT(*) FROM tombstones WHERE expires_at_secs > ?1",
                now_secs,
            )
            .await?;
        Ok(AdminStats {
            active_installations,
            active_tombstones,
            expired_installations,
        })
    }

    async fn read_table_columns(&self, table: &str) -> Result<Vec<String>, StoreError> {
        let query = match table {
            "installations" => "PRAGMA table_info(installations)",
            "tombstones" => "PRAGMA table_info(tombstones)",
            _ => return Err(StoreError::Storage("unknown table".to_owned())),
        };
        let mut rows = self
            .connection
            .query(query, ())
            .await
            .map_err(storage_err)?;
        let mut columns = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage_err)? {
            columns.push(row.get::<String>(1).map_err(storage_err)?);
        }
        Ok(columns)
    }

    async fn count_query(&self, sql: &str, now_secs: i64) -> Result<u64, StoreError> {
        let mut rows = self
            .connection
            .query(sql, turso::params![now_secs])
            .await
            .map_err(storage_err)?;
        let Some(row) = rows.next().await.map_err(storage_err)? else {
            return Ok(0);
        };
        let count = row.get::<i64>(0).map_err(storage_err)?;
        u64::try_from(count).map_err(|_| StoreError::Storage("negative row count".to_owned()))
    }

    async fn find_installation(
        &self,
        installation_id: &str,
    ) -> Result<Option<StoredInstallation>, StoreError> {
        let mut rows = self
            .connection
            .query(
                "SELECT token_hash, report_sequence, payload_hash, expires_at_secs \
                 FROM installations WHERE installation_id = ?1",
                turso::params![installation_id],
            )
            .await
            .map_err(storage_err)?;
        let Some(row) = rows.next().await.map_err(storage_err)? else {
            return Ok(None);
        };
        Ok(Some(StoredInstallation {
            token_hash: row.get::<Vec<u8>>(0).map_err(storage_err)?,
            report_sequence: row.get::<i64>(1).map_err(storage_err)?,
            payload_hash: row.get::<Vec<u8>>(2).map_err(storage_err)?,
            expires_at_secs: row.get::<i64>(3).map_err(storage_err)?,
        }))
    }

    async fn find_tombstone(
        &self,
        installation_id: &str,
    ) -> Result<Option<(Vec<u8>, i64)>, StoreError> {
        let mut rows = self
            .connection
            .query(
                "SELECT token_hash, expires_at_secs FROM tombstones WHERE installation_id = ?1",
                turso::params![installation_id],
            )
            .await
            .map_err(storage_err)?;
        let Some(row) = rows.next().await.map_err(storage_err)? else {
            return Ok(None);
        };
        Ok(Some((
            row.get::<Vec<u8>>(0).map_err(storage_err)?,
            row.get::<i64>(1).map_err(storage_err)?,
        )))
    }

    async fn finish_tx<T>(&self, result: Result<T, StoreError>) -> Result<T, StoreError> {
        match result {
            Ok(value) => {
                self.connection
                    .execute_batch("COMMIT")
                    .await
                    .map_err(storage_err)?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK").await;
                Err(error)
            }
        }
    }
}

struct StoredInstallation {
    token_hash: Vec<u8>,
    report_sequence: i64,
    payload_hash: Vec<u8>,
    expires_at_secs: i64,
}

async fn initialize_schema(connection: &turso::Connection) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS installations (
                installation_id TEXT PRIMARY KEY NOT NULL,
                token_hash BLOB NOT NULL,
                report_sequence INTEGER NOT NULL,
                payload_hash BLOB NOT NULL,
                payload_json TEXT NOT NULL,
                updated_at_secs INTEGER NOT NULL,
                expires_at_secs INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_installations_expires
                ON installations (expires_at_secs);
            CREATE TABLE IF NOT EXISTS tombstones (
                installation_id TEXT PRIMARY KEY NOT NULL,
                token_hash BLOB NOT NULL,
                deleted_at_secs INTEGER NOT NULL,
                expires_at_secs INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tombstones_expires
                ON tombstones (expires_at_secs);",
        )
        .await?;
    Ok(())
}

pub fn sha256_bytes(input: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hasher.finalize().into()
}

pub fn hashes_match(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn storage_err(error: impl std::fmt::Display) -> StoreError {
    StoreError::Storage(error.to_string())
}
