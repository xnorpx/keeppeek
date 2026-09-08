use super::{Action, Failure, Job, load, validate_action};
use crate::storage::catalog::maintenance::{ReadRequest, check_deadline};
use crate::storage::catalog::{BUSY_TIMEOUT, RecordingCatalogHandle, SearchCommand};
use std::{sync::mpsc, time::Instant};

impl RecordingCatalogHandle {
    /// Reads at most sixteen actor-owned job records after an exclusive job ID.
    ///
    /// # Errors
    /// Rejects invalid actors/cursors, corrupt history, and unavailable or expired reads.
    pub fn recording_deletion_jobs(&self, actor: &str, after: &str) -> anyhow::Result<Vec<Job>> {
        super::super::validate_identifier(actor)?;
        if !after.is_empty() {
            validate_action(
                actor,
                &Action::Read {
                    id: after.to_owned(),
                },
            )?;
        }
        let deadline = Instant::now() + BUSY_TIMEOUT;
        let (reply, response) = mpsc::sync_channel(1);
        self.search_tx
            .try_send(SearchCommand::Maintenance(ReadRequest::Jobs {
                actor: actor.to_owned(),
                after: after.to_owned(),
                deadline,
                reply,
            }))
            .map_err(|_| Failure::Unavailable)?;
        response
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failure::Unavailable)?
            .map_err(|error| {
                error
                    .downcast_ref::<Failure>()
                    .copied()
                    .unwrap_or(Failure::Unavailable)
                    .into()
            })
    }
}

pub(in crate::storage::catalog) async fn read(
    connection: &turso::Connection,
    actor: &str,
    after: &str,
    deadline: Instant,
) -> anyhow::Result<Vec<Job>> {
    check_deadline(deadline)?;
    connection.execute_batch("BEGIN").await?;
    let result = async {
        let mut rows = connection.query(
            "SELECT id FROM recording_maintenance_intents WHERE actor = ?1 AND id > ?2 ORDER BY id LIMIT 16",
            turso::params![actor, after],
        ).await?;
        let mut ids: Vec<String> = Vec::with_capacity(16);
        while let Some(row) = rows.next().await? {
            check_deadline(deadline)?;
            anyhow::ensure!(ids.len() < 16, Failure::Invalid);
            ids.push(row.get(0)?);
        }
        drop(rows);
        let mut jobs = Vec::with_capacity(ids.len());
        for id in ids {
            check_deadline(deadline)?;
            jobs.push(load(connection, actor, &id, deadline).await?.0);
        }
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(jobs)
    }.await;
    if result.is_err() && !connection.is_autocommit()? {
        connection.execute_batch("ROLLBACK").await?;
    }
    result
}
