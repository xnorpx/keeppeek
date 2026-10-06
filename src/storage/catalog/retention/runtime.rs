//! Restartable policy activation and bounded canonical-evidence reevaluation.

use anyhow::{Context, Result, ensure};
use std::time::Instant;

use super::super::RecordingCatalogHandle;
use crate::storage::retention::settings::Settings;

mod expiry;
mod schema;
mod work;

const SETTINGS_BYTES_MAX: usize = 524_288;
const RECORDS_PER_CALL_MAX: usize = 8;

/// Work observed during a bounded reconciliation call. Quarantined files remain undeletable.
#[derive(Debug, Default, Clone, Copy)]
pub struct Progress {
    pub evaluated: u32,
    pub quarantined: u32,
    pub pending: bool,
    pub activation_pending: bool,
}

pub(super) async fn initialize(connection: &turso::Connection) -> Result<()> {
    schema::initialize(connection).await
}

impl RecordingCatalogHandle {
    /// Requests a validated policy change. Old obligations finish before the new policy activates.
    /// New automatic deletion admission is fenced while the change or initial backfill is pending.
    /// Returns false when a previously accepted transition must finish before this request.
    pub fn request_retention_settings(&self, settings: Option<&Settings>) -> Result<bool> {
        if let Some(settings) = settings {
            settings.validate()?;
        }
        let json = settings.map(serde_json::to_string).transpose()?;
        ensure!(
            json.as_ref()
                .is_none_or(|json| json.len() <= SETTINGS_BYTES_MAX),
            "retention settings metadata limit exceeded"
        );
        let started = Instant::now();
        self.check_retention_available(started)?;
        let owner = self
            .retention
            .upgrade()
            .context("retention catalog is closed")?;
        let connection = owner
            .connection
            .try_lock()
            .map_err(|_| anyhow::anyhow!("retention catalog is busy or poisoned"))?;
        pollster::block_on(async {
            connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                owner.authority.verify_transaction(&connection)?;
                let accepted = expiry::request(&connection, json.as_deref()).await?;
                self.check_retention_available(started)?;
                connection.execute_batch("COMMIT").await?;
                Ok(accepted)
            }
            .await;
            if result.is_err() {
                connection
                    .execute_batch("ROLLBACK")
                    .await
                    .context("rollback retention settings request")?;
            }
            result
        })
    }

    /// Processes at most eight catalog work items, each in a separately budgeted transaction.
    pub fn reconcile_retention_runtime(&self, max_records: usize) -> Result<Progress> {
        ensure!(
            (1..=RECORDS_PER_CALL_MAX).contains(&max_records),
            "retention runtime batch must be 1 to 8"
        );
        self.check_retention_available(Instant::now())?;
        let owner = self
            .retention
            .upgrade()
            .context("retention catalog is closed")?;
        let connection = owner
            .connection
            .try_lock()
            .map_err(|_| anyhow::anyhow!("retention catalog is busy or poisoned"))?;
        let mut progress = Progress::default();
        for _ in 0..max_records {
            let started = Instant::now();
            let step = pollster::block_on(async {
                connection.execute_batch("BEGIN IMMEDIATE").await?;
                let result = async {
                    owner.authority.verify_transaction(&connection)?;
                    let step = work::step(&connection).await?;
                    self.check_retention_available(started)?;
                    connection.execute_batch("COMMIT").await?;
                    anyhow::Ok(step)
                }
                .await;
                if result.is_err() {
                    connection
                        .execute_batch("ROLLBACK")
                        .await
                        .context("rollback retention runtime step")?;
                }
                result
            })?;
            progress.evaluated += step.evaluated;
            progress.quarantined += step.quarantined;
            progress.pending = step.pending;
            if !step.pending {
                break;
            }
        }
        progress.activation_pending = pollster::block_on(expiry::activation_pending(&connection))?;
        Ok(progress)
    }

    /// Returns at most eight current-generation expiry candidates using a durable indexed cursor.
    /// A deletion owner must independently admit each candidate under current authority and holds.
    pub(crate) fn expired_retention_candidates(&self, max_records: usize) -> Result<Vec<String>> {
        ensure!(
            (1..=RECORDS_PER_CALL_MAX).contains(&max_records),
            "retention expiry batch must be 1 to 8"
        );
        let started = Instant::now();
        self.check_retention_available(started)?;
        let owner = self
            .retention
            .upgrade()
            .context("retention catalog is closed")?;
        let connection = owner
            .connection
            .try_lock()
            .map_err(|_| anyhow::anyhow!("retention catalog is busy or poisoned"))?;
        pollster::block_on(async {
            connection.execute_batch("BEGIN IMMEDIATE").await?;
            let result = async {
                owner.authority.verify_transaction(&connection)?;
                let candidates = expiry::candidates(&connection, max_records).await?;
                self.check_retention_available(started)?;
                connection.execute_batch("COMMIT").await?;
                anyhow::Ok(candidates)
            }
            .await;
            if result.is_err() {
                connection
                    .execute_batch("ROLLBACK")
                    .await
                    .context("rollback retention expiry cursor")?;
            }
            result
        })
    }
}

pub(in crate::storage::catalog) async fn ensure_admission_ready(
    connection: &turso::Connection,
) -> Result<()> {
    let mut rows = connection
        .query(
            "SELECT request_pending,complete FROM recording_retention_runtime WHERE singleton=1",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .context("retention runtime state missing")?;
    ensure!(
        row.get::<i64>(0)? == 0 && row.get::<i64>(1)? == 1,
        "retention activation fences automatic cleanup"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
